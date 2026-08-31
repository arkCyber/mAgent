//! mAgent Simulator for Testing
//!
//! Provides a realistic simulation of the mAgent embedded AI agent
//! for testing without actual hardware. Uses real HTTP client to
//! connect to Ollama for actual LLM reasoning.

#![cfg(feature = "std")]

// `lib.rs` is `#![no_std]`, so the standard prelude isn't in scope for this
// file. When the `std` feature is enabled `lib.rs` re-imports `std` with
// `#[macro_use]`, which brings `format!`, `vec!`, `println!` and friends
// into scope throughout the crate. We also import the standard collection
// types directly so the rest of the file can write `String` / `Vec` without
// qualifying them.
use crate::error::{AgentError, Result};
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

/// Simulated sensor types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SimSensorType {
    /// Ambient temperature (°C).
    Temperature,
    /// 3-axis accelerometer (m/s²).
    Accelerometer,
    /// Relative humidity (%RH).
    Humidity,
    /// Atmospheric pressure (hPa).
    Pressure,
    /// Ambient light level (lux).
    Light,
    /// Instantaneous heart rate (BPM).
    HeartRate,
    /// Heart-rate variability (RMSSD, ms).
    Hrv,
    /// Blood glucose (mg/dL).
    Glucose,
    /// Single-lead ECG waveform sample.
    Ecg,
    /// Computed stress level index.
    Stress,
}

/// Simulated GPIO states
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GpioPinState {
    /// Logic-low (0 V).
    Low,
    /// Logic-high (Vcc).
    High,
}

/// Simulated flash storage
pub struct SimFlashStorage {
    data: [u8; 65536],
    writes: usize,
}

impl SimFlashStorage {
    /// Create new simulated flash
    pub fn new() -> Self {
        Self {
            data: [0xFF; 65536],
            writes: 0,
        }
    }

    /// Read from flash
    pub fn read(&self, address: u32, length: usize) -> Result<Vec<u8>> {
        let start = address as usize;
        if start + length > 65536 {
            return Err(AgentError::StorageReadFailed {
                address,
                reason: crate::error::StorageError::BadAddress,
            });
        }
        Ok(self.data[start..start + length].to_vec())
    }

    /// Write to flash
    pub fn write(&mut self, address: u32, data: &[u8]) -> Result<()> {
        let start = address as usize;
        if start + data.len() > 65536 {
            return Err(AgentError::StorageWriteFailed {
                address,
                reason: crate::error::StorageError::BadAddress,
            });
        }
        for (i, &byte) in data.iter().enumerate() {
            self.data[start + i] = byte;
        }
        self.writes += 1;
        Ok(())
    }

    /// Get write count
    pub fn write_count(&self) -> usize {
        self.writes
    }

    /// Erase a sector (4KB)
    pub fn erase_sector(&mut self, sector: u32) -> Result<()> {
        let start = sector as usize * 4096;
        for i in 0..4096.min(65536 - start) {
            self.data[start + i] = 0xFF;
        }
        Ok(())
    }
}

impl Default for SimFlashStorage {
    fn default() -> Self {
        Self::new()
    }
}

/// Simulated sensor readings with realistic variations
pub struct SimSensorManager {
    temperature_base: f32,
    humidity_base: f32,
    pressure_base: f32,
    accel_x: f32,
    accel_y: f32,
    accel_z: f32,
    iteration: usize,
}

impl SimSensorManager {
    /// Create new sensor manager
    pub fn new() -> Self {
        Self {
            temperature_base: 23.5,
            humidity_base: 55.0,
            pressure_base: 1013.25,
            accel_x: 0.0,
            accel_y: 0.0,
            accel_z: 9.8,
            iteration: 0,
        }
    }

    /// Read temperature in Celsius
    pub fn read_temperature(&mut self) -> f32 {
        self.iteration += 1;
        let variation = ((self.iteration as f32 * 0.1).sin() * 2.0)
            + ((self.iteration as f32 * 0.3).cos() * 0.5);
        self.temperature_base + variation
    }

    /// Read humidity in percentage
    pub fn read_humidity(&mut self) -> f32 {
        self.iteration += 1;
        let variation = ((self.iteration as f32 * 0.05).sin() * 5.0) + 2.0;
        self.humidity_base + variation
    }

    /// Read pressure in hPa
    pub fn read_pressure(&mut self) -> f32 {
        self.iteration += 1;
        let variation = (self.iteration as f32 * 0.02).sin() * 2.0;
        self.pressure_base + variation
    }

    /// Read accelerometer in g
    pub fn read_accelerometer(&mut self) -> (f32, f32, f32) {
        self.iteration += 1;
        let noise = || (self.iteration as f32 * 17.3).sin() * 0.01;
        (
            self.accel_x + noise(),
            self.accel_y + noise(),
            self.accel_z + noise(),
        )
    }

    /// Read light level in lux
    pub fn read_light(&mut self) -> f32 {
        self.iteration += 1;
        let cycle = (self.iteration as f32 * 0.01).sin() * 500.0 + 500.0;
        cycle.max(10.0)
    }
}

impl Default for SimSensorManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Simulated GPIO controller
pub struct SimGpioController {
    pins: [GpioPinState; 32],
}

impl SimGpioController {
    /// Create new GPIO controller
    pub fn new() -> Self {
        Self {
            pins: [GpioPinState::Low; 32],
        }
    }

    /// Set pin state
    pub fn set_pin(&mut self, pin: u8, state: GpioPinState) -> Result<()> {
        if pin >= 32 {
            return Err(AgentError::GpioOperationFailed {
                pin,
                operation: crate::error::GpioOperation::Write,
            });
        }
        self.pins[pin as usize] = state;
        Ok(())
    }

    /// Get pin state
    pub fn get_pin(&self, pin: u8) -> Result<GpioPinState> {
        if pin >= 32 {
            return Err(AgentError::GpioOperationFailed {
                pin,
                operation: crate::error::GpioOperation::Read,
            });
        }
        Ok(self.pins[pin as usize])
    }

    /// Toggle pin state
    pub fn toggle_pin(&mut self, pin: u8) -> Result<()> {
        let current = self.get_pin(pin)?;
        let new_state = match current {
            GpioPinState::Low => GpioPinState::High,
            GpioPinState::High => GpioPinState::Low,
        };
        self.set_pin(pin, new_state)
    }
}

impl Default for SimGpioController {
    fn default() -> Self {
        Self::new()
    }
}

/// Simulated BLE interface
pub struct SimBleInterface {
    connected: bool,
    messages_sent: usize,
    last_message: Option<String>,
}

impl SimBleInterface {
    /// Create new BLE interface
    pub fn new() -> Self {
        Self {
            connected: false,
            messages_sent: 0,
            last_message: None,
        }
    }

    /// Connect to BLE gateway
    pub fn connect(&mut self) -> Result<()> {
        self.connected = true;
        Ok(())
    }

    /// Disconnect
    pub fn disconnect(&mut self) {
        self.connected = false;
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Send data via BLE
    pub fn send(&mut self, data: &str) -> Result<()> {
        if !self.connected {
            return Err(AgentError::NetworkConnectionFailed {
                reason: crate::error::NetworkError::ConnectionRefused,
            });
        }
        self.messages_sent += 1;
        self.last_message = Some(data.to_string());
        Ok(())
    }

    /// Get message count
    pub fn message_count(&self) -> usize {
        self.messages_sent
    }

    /// Get last message
    pub fn last_message(&self) -> Option<&str> {
        self.last_message.as_deref()
    }
}

impl Default for SimBleInterface {
    fn default() -> Self {
        Self::new()
    }
}

/// Complete agent simulator with health support
pub struct AgentSimulator {
    /// Simulated flash storage backing the agent's persistence layer.
    pub flash: SimFlashStorage,
    /// Simulated sensor manager producing deterministic readings.
    pub sensors: SimSensorManager,
    /// Simulated GPIO controller.
    pub gpio: SimGpioController,
    /// Simulated BLE interface for over-the-air traffic.
    pub ble: SimBleInterface,
    /// Health sensor state
    pub health_state: HealthSimState,
}

/// Health simulation state
pub struct HealthSimState {
    /// Current exercise state
    pub is_exercising: bool,
    /// Minutes elapsed in simulation
    pub minutes_elapsed: u32,
    /// Current stress level simulation
    pub stress_level: u8,
    /// Last heart rate reading
    pub last_hr: u16,
    /// Last HRV reading
    pub last_hrv: f32,
    /// Last glucose reading
    pub last_glucose: f32,
    /// Hours since meal (for glucose simulation)
    pub hours_since_meal: f32,
}

impl HealthSimState {
    /// Default-construct a state with no in-progress exercise, 30 %
    /// baseline stress, a 72 BPM resting heart rate, HRV of 55 ms,
    /// fasting-glucose 100 mg/dL, and "2 h since last meal".
    pub fn new() -> Self {
        Self {
            is_exercising: false,
            minutes_elapsed: 0,
            stress_level: 30,
            last_hr: 72,
            last_hrv: 55.0,
            last_glucose: 100.0,
            hours_since_meal: 2.0,
        }
    }
}

impl Default for HealthSimState {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentSimulator {
    /// Create new simulator
    pub fn new() -> Self {
        Self {
            flash: SimFlashStorage::new(),
            sensors: SimSensorManager::new(),
            gpio: SimGpioController::new(),
            ble: SimBleInterface::new(),
            health_state: HealthSimState::new(),
        }
    }

    /// Execute a tool call
    pub fn execute_tool(&mut self, tool_name: &str, args: &str) -> Result<String> {
        match tool_name {
            "read_sensor" => {
                let sensor = if args.contains("temperature") {
                    SimSensorType::Temperature
                } else if args.contains("accelerometer") {
                    SimSensorType::Accelerometer
                } else if args.contains("humidity") {
                    SimSensorType::Humidity
                } else if args.contains("pressure") {
                    SimSensorType::Pressure
                } else if args.contains("light") {
                    SimSensorType::Light
                } else if args.contains("heart_rate") {
                    SimSensorType::HeartRate
                } else if args.contains("hrv") {
                    SimSensorType::Hrv
                } else if args.contains("glucose") {
                    SimSensorType::Glucose
                } else if args.contains("ecg") {
                    SimSensorType::Ecg
                } else if args.contains("stress") {
                    SimSensorType::Stress
                } else {
                    return Err(AgentError::ConfigurationError {
                        field: "sensor",
                        reason: crate::error::ConfigError::MissingField,
                    });
                };

                let result = match sensor {
                    SimSensorType::Temperature => {
                        let temp = self.sensors.read_temperature();
                        format!("Temperature: {:.1}°C", temp)
                    }
                    SimSensorType::Accelerometer => {
                        let (x, y, z) = self.sensors.read_accelerometer();
                        // Note: the `f` float-formatting flag is not available
                        // when building under `#![no_std]` (the `format_args!`
                        // macro falls back to the smaller `core::fmt` that
                        // doesn't ship float support). We rely on the natural
                        // `Display` impl for `f32`, which gives 6 fractional
                        // digits by default.
                        format!("Accelerometer: X={}g Y={}g Z={}g", x, y, z)
                    }
                    SimSensorType::Humidity => {
                        let humidity = self.sensors.read_humidity();
                        format!("Humidity: {:.1}%", humidity)
                    }
                    SimSensorType::Pressure => {
                        let pressure = self.sensors.read_pressure();
                        format!("Pressure: {:.1} hPa", pressure)
                    }
                    SimSensorType::Light => {
                        let light = self.sensors.read_light();
                        format!("Light: {:.1} lux", light)
                    }
                    SimSensorType::HeartRate => {
                        let hr = self.simulate_heart_rate();
                        format!("Heart Rate: {} BPM, HRV: {:.1} ms", hr.0, hr.1)
                    }
                    SimSensorType::Hrv => {
                        let hrv = self.simulate_hrv();
                        format!("HRV: {:.1} ms, Stress Level: {}", hrv.0, hrv.1)
                    }
                    SimSensorType::Glucose => {
                        let glucose = self.simulate_glucose();
                        format!("Glucose: {:.0} mg/dL, Trend: {}", glucose.0, glucose.1)
                    }
                    SimSensorType::Ecg => {
                        let ecg = self.simulate_ecg();
                        format!(
                            "ECG: HR={} BPM, Rhythm={}, Quality={}%",
                            ecg.0, ecg.1, ecg.2
                        )
                    }
                    SimSensorType::Stress => {
                        let stress = self.calculate_stress();
                        format!(
                            "Stress Level: {} ({}), HRV: {:.1} ms",
                            stress.0, stress.1, stress.2
                        )
                    }
                };

                Ok(result)
            }
            "write_gpio" => {
                let pin = extract_int_arg(args, "pin").unwrap_or(0) as u8;
                let state = if args.contains("high") {
                    GpioPinState::High
                } else {
                    GpioPinState::Low
                };
                self.gpio.set_pin(pin, state)?;
                let state_str = if matches!(state, GpioPinState::High) {
                    "high"
                } else {
                    "low"
                };
                Ok(format!("GPIO pin {} set to {}", pin, state_str))
            }
            "flash_read" => {
                let address = extract_int_arg(args, "address").unwrap_or(0) as u32;
                let data = self.flash.read(address, 64)?;
                let hex: String = data.iter().take(16).map(|b| format!("{:02X}", b)).collect();
                Ok(format!("Flash read at 0x{:04X}: {}", address, hex))
            }
            "flash_write" => {
                let address = extract_int_arg(args, "address").unwrap_or(0) as u32;
                let data = format!("data at {}", address);
                self.flash.write(address, data.as_bytes())?;
                Ok(format!(
                    "Wrote {} bytes to flash at 0x{:04X}",
                    data.len(),
                    address
                ))
            }
            "ble_send" => {
                self.ble.send(args)?;
                Ok(format!("Sent {} bytes via BLE", args.len()))
            }
            "web_search" => {
                // Delegate to the dedicated web module so the heavy
                // lifting (regex, reqwest, HTML stripping) lives in
                // one place. Tool failures are surfaced as `Ok` with
                // a leading "error:" prefix so the LLM sees the
                // failure (the agent loop matches on the prefix).
                match crate::web::web_search(args) {
                    Ok(s) => Ok(s),
                    Err(e) => Ok(format!("error: web_search failed: {e}")),
                }
            }
            "fetch_url" => match crate::web::fetch_url(args) {
                Ok(s) => Ok(s),
                Err(e) => Ok(format!("error: fetch_url failed: {e}")),
            },
            "webpage_summary" => match crate::web::webpage_summary(args) {
                Ok(s) => Ok(s),
                Err(e) => Ok(format!("error: webpage_summary failed: {e}")),
            },
            "get_weather" => match crate::web::get_weather(args) {
                Ok(s) => Ok(s),
                Err(e) => Ok(format!("error: get_weather failed: {e}")),
            },
            _ => Err(AgentError::ConfigurationError {
                field: "tool",
                reason: crate::error::ConfigError::MissingField,
            }),
        }
    }

    /// Simulate heart rate based on exercise state
    fn simulate_heart_rate(&mut self) -> (u16, f32) {
        self.health_state.minutes_elapsed += 1;

        let base_hr = if self.health_state.is_exercising {
            140
        } else {
            70
        };
        let variation = ((self.health_state.minutes_elapsed as f32 * 0.1).sin() * 10.0) as i16;
        let hr = (base_hr as i16 + variation).clamp(50, 200) as u16;

        let base_hrv = if self.health_state.is_exercising {
            20.0
        } else {
            55.0
        };
        let hrv = base_hrv + ((self.health_state.minutes_elapsed as f32 * 0.2).sin() * 5.0);

        self.health_state.last_hr = hr;
        self.health_state.last_hrv = hrv;

        (hr, hrv)
    }

    /// Simulate HRV based on stress level
    fn simulate_hrv(&mut self) -> (f32, u8) {
        // HRV decreases with stress
        let base_hrv = 80.0 - (self.health_state.stress_level as f32 * 0.5);
        let hrv = base_hrv + ((self.health_state.minutes_elapsed as f32 * 0.15).sin() * 5.0);
        self.health_state.last_hrv = hrv;
        (hrv, self.health_state.stress_level)
    }

    /// Simulate blood glucose
    fn simulate_glucose(&mut self) -> (f32, &'static str) {
        self.health_state.hours_since_meal += 0.5;
        let base = 100.0;
        let rise = (self.health_state.hours_since_meal * 20.0).min(50.0);
        let variation = (self.health_state.hours_since_meal * 0.5).sin() * 10.0;
        let glucose = base + rise + variation;
        self.health_state.last_glucose = glucose;

        let trend = if self.health_state.hours_since_meal < 2.0 {
            "Rising"
        } else if self.health_state.hours_since_meal < 4.0 {
            "Stable"
        } else {
            "Falling"
        };

        (glucose, trend)
    }

    /// Simulate ECG data
    fn simulate_ecg(&mut self) -> (u16, &'static str, u8) {
        let (hr, _) = self.simulate_heart_rate();
        let rhythm = if self.health_state.stress_level > 70 {
            "Irregular"
        } else {
            "Normal"
        };
        (hr, rhythm, 95)
    }

    /// Calculate stress from HRV
    fn calculate_stress(&self) -> (u8, &'static str, f32) {
        let hrv = self.health_state.last_hrv;
        let (level, desc) = if hrv >= 80.0 {
            (20u8, "Low")
        } else if hrv >= 50.0 {
            (50u8, "Moderate")
        } else if hrv >= 25.0 {
            (75u8, "High")
        } else {
            (95u8, "Very High")
        };
        (level, desc, hrv)
    }

    /// Set exercise state
    pub fn set_exercising(&mut self, exercising: bool) {
        self.health_state.is_exercising = exercising;
    }

    /// Set stress level
    pub fn set_stress_level(&mut self, level: u8) {
        self.health_state.stress_level = level.clamp(0, 100);
    }

    /// Set hours since meal
    pub fn set_hours_since_meal(&mut self, hours: f32) {
        self.health_state.hours_since_meal = hours;
    }

    /// Get system status
    pub fn get_status(&self) -> String {
        format!(
            "Simulator Status:\n  Flash writes: {}\n  BLE messages: {}\n  BLE connected: {}\n  Health State: HR={} BPM, HRV={:.1} ms, Stress={}",
            self.flash.write_count(),
            self.ble.message_count(),
            self.ble.is_connected(),
            self.health_state.last_hr,
            self.health_state.last_hrv,
            self.health_state.stress_level
        )
    }
}

impl Default for AgentSimulator {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract integer argument from string
fn extract_int_arg(s: &str, key: &str) -> Option<i32> {
    let prefix = format!("{}=", key);
    s.find(&prefix).and_then(|pos| {
        let start = pos + prefix.len();
        let end = s[start..]
            .find(|c: char| !c.is_ascii_digit())
            .map(|i| start + i)
            .unwrap_or(s.len());
        s[start..end].parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The LLM-shaped payload that `agent_runner::execute_tool`
    /// serialises via `serde_json::to_string`. We assert that the
    /// web tools reach the HTTP layer instead of bouncing back
    /// with "missing url" — the regression that motivated the JSON
    /// parser in `web::extract_query`.
    ///
    /// We don't actually hit the network here: the `fetch_url` path
    /// rejects any URL that doesn't start with `http(s)`, so the
    /// empty-string URL gives us a deterministic "refusing non-http(s)
    /// URL" error which is the layer we want to reach.
    #[test]
    fn execute_tool_routes_web_search_to_web_module() {
        let mut sim = AgentSimulator::new();
        // `query=` ... `=http` makes a key=value arg that
        // `extract_query` can parse. The actual search will fail
        // (no network in unit tests), but the failure surface
        // proves the routing worked.
        let result = sim.execute_tool("web_search", "query=rust");
        assert!(result.is_ok());
        let body = result.unwrap();
        // Either an answer (network is reachable) or a placeholder
        // error string — both prove we reached `web::web_search`.
        assert!(
            body.starts_with('[') || body.starts_with("error: web_search failed"),
            "expected JSON hits or error, got {body}"
        );
    }

    #[test]
    fn execute_tool_routes_fetch_url_with_json_args() {
        // The LLM emits arguments as `{"url":"..."}`. The previous
        // implementation fed that string to a `key=value` parser and
        // always returned "missing url" — the dispatch was 100%
        // broken for any LLM-driven caller. We verify the round-trip
        // reaches the URL validator (which rejects non-http(s)).
        let mut sim = AgentSimulator::new();
        let result = sim.execute_tool("fetch_url", r#"{"url":""}"#);
        assert!(result.is_ok());
        let body = result.unwrap();
        assert!(
            body.starts_with("error: fetch_url failed"),
            "expected fetch_url to be called, got {body}"
        );
        // The error message should reference the URL scheme check.
        assert!(
            body.contains("refusing non-http"),
            "expected URL scheme rejection, got {body}"
        );
    }

    #[test]
    fn execute_tool_routes_fetch_url_with_kv_args() {
        // The embedded planner (no JSON support) emits `url=...`.
        // This path also has to keep working.
        let mut sim = AgentSimulator::new();
        let result = sim.execute_tool("fetch_url", "url=");
        assert!(result.is_ok());
        let body = result.unwrap();
        assert!(body.contains("error: fetch_url failed"));
    }

    #[test]
    fn execute_tool_routes_webpage_summary_with_json_args() {
        let mut sim = AgentSimulator::new();
        let result = sim.execute_tool(
            "webpage_summary",
            r#"{"url":"https://example.com","query":"test"}"#,
        );
        // Network may or may not be available in tests; either way
        // we should NOT see "missing url" because the JSON parser
        // finds the URL.
        assert!(result.is_ok());
        let body = result.unwrap();
        assert!(
            !body.contains("missing 'url'"),
            "webpage_summary should have parsed the JSON url, got {body}"
        );
    }

    #[test]
    fn execute_tool_routes_get_weather_with_json_args() {
        let mut sim = AgentSimulator::new();
        // Empty city is a cheap, network-independent failure that still
        // proves the dispatch reached `web::get_weather` (and parsed the
        // JSON `city` arg) rather than bouncing back as an unknown tool.
        let result = sim.execute_tool("get_weather", r#"{"city":""}"#);
        assert!(result.is_ok());
        let body = result.unwrap();
        assert!(
            body.starts_with("error: get_weather failed"),
            "expected get_weather to be called, got {body}"
        );
        assert!(
            body.contains("empty city"),
            "expected the empty-city guard, got {body}"
        );
    }

    #[test]
    fn execute_tool_routes_get_weather_with_kv_args() {
        let mut sim = AgentSimulator::new();
        // The embedded planner (no JSON support) emits `city=...`.
        let result = sim.execute_tool("get_weather", "city=");
        assert!(result.is_ok());
        let body = result.unwrap();
        assert!(body.contains("error: get_weather failed"));
    }

    #[test]
    fn sim_flash_storage_read_write_erase() {
        let mut flash = SimFlashStorage::new();
        assert_eq!(flash.write_count(), 0);
        // Initial state is all 0xFF.
        assert_eq!(flash.read(0, 4).unwrap(), vec![0xFF, 0xFF, 0xFF, 0xFF]);
        // Write and read back.
        flash.write(0, &[1, 2, 3, 4]).unwrap();
        assert_eq!(flash.write_count(), 1);
        assert_eq!(flash.read(0, 4).unwrap(), vec![1, 2, 3, 4]);
        // Overlapping write.
        flash.write(2, &[9, 9]).unwrap();
        assert_eq!(flash.read(0, 4).unwrap(), vec![1, 2, 9, 9]);
        // Erase resets a sector to 0xFF.
        flash.erase_sector(0).unwrap();
        assert_eq!(flash.read(0, 4).unwrap(), vec![0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn sim_flash_storage_rejects_out_of_bounds() {
        let mut flash = SimFlashStorage::new();
        // Read past the end → StorageReadFailed.
        assert!(matches!(
            flash.read(65530, 10),
            Err(AgentError::StorageReadFailed { .. })
        ));
        // Write past the end → StorageWriteFailed.
        assert!(matches!(
            flash.write(65534, &[1, 2, 3]),
            Err(AgentError::StorageWriteFailed { .. })
        ));
    }

    #[test]
    fn sim_gpio_controller_set_get_toggle() {
        let mut gpio = SimGpioController::new();
        assert_eq!(gpio.get_pin(0).unwrap(), GpioPinState::Low);
        gpio.set_pin(5, GpioPinState::High).unwrap();
        assert_eq!(gpio.get_pin(5).unwrap(), GpioPinState::High);
        gpio.toggle_pin(5).unwrap();
        assert_eq!(gpio.get_pin(5).unwrap(), GpioPinState::Low);
        gpio.toggle_pin(5).unwrap();
        assert_eq!(gpio.get_pin(5).unwrap(), GpioPinState::High);
        // Out-of-range pins are rejected for read and write.
        assert!(matches!(
            gpio.get_pin(32),
            Err(AgentError::GpioOperationFailed { .. })
        ));
        assert!(matches!(
            gpio.set_pin(32, GpioPinState::High),
            Err(AgentError::GpioOperationFailed { .. })
        ));
    }

    #[test]
    fn sim_ble_interface_connect_send() {
        let mut ble = SimBleInterface::new();
        assert!(!ble.is_connected());
        // Send before connect is refused.
        assert!(matches!(
            ble.send("hi"),
            Err(AgentError::NetworkConnectionFailed { .. })
        ));
        ble.connect().unwrap();
        assert!(ble.is_connected());
        ble.send("hello").unwrap();
        ble.send("world").unwrap();
        assert_eq!(ble.message_count(), 2);
        assert_eq!(ble.last_message(), Some("world"));
        ble.disconnect();
        assert!(!ble.is_connected());
        // Send after disconnect is refused again.
        assert!(ble.send("late").is_err());
    }

    #[test]
    fn sim_sensor_manager_reads_realistic_values() {
        let mut s = SimSensorManager::new();
        // Temperature near base (23.5) with small bounded variation.
        let t = s.read_temperature();
        assert!((18.0..=29.0).contains(&t), "temp {t}");
        // Humidity near base (55).
        let h = s.read_humidity();
        assert!((45.0..=65.0).contains(&h), "humidity {h}");
        // Pressure near base (1013).
        let p = s.read_pressure();
        assert!((1000.0..=1020.0).contains(&p), "pressure {p}");
        // Accelerometer: z ≈ 9.8 with small noise, x/y ≈ 0.
        let (x, y, z) = s.read_accelerometer();
        assert!((z - 9.8).abs() < 0.1, "z {z}");
        assert!(x.abs() < 0.1);
        assert!(y.abs() < 0.1);
        // Light within its cycle range.
        let l = s.read_light();
        assert!((10.0..=1000.0).contains(&l), "light {l}");
        // Readings vary across iterations rather than being constant.
        let t2 = s.read_temperature();
        assert!(t != t2, "temperature should vary between reads");
    }
}

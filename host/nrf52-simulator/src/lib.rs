//! nRF52840 Smartwatch AI Agent Simulator
//!
//! A complete simulation environment for testing smartwatch AI agents
//! running on nRF52840 hardware.
//!
//! ## Features
//!
//! - **nRF52840 Hardware Simulation**: CPU, Flash, RAM, GPIO, BLE
//! - **Smartwatch Sensors**: Temperature, Accelerometer, Heart Rate, SpO2, Steps
//! - **AI Agent**: ReAct-based agent with tool execution
//! - **Voice**: Speech Recognition (STT) and Speech Synthesis (TTS)
//! - **Network**: Web search and summarization
//! - **Smart Home**: IoT device control
//! - **Power Management**: Battery, power modes
//! - **Ollama Integration**: Real LLM reasoning when available
//!
//! ## Quick Start
//!
//! ```rust
//! use nrf52_simulator::{SmartwatchAgent, run_demos};
//!
//! // Run the agent
//! let mut agent = SmartwatchAgent::with_defaults();
//! let result = agent.run("Check my heart rate");
//! ```

use serde::{Deserialize, Serialize};
use rand::Rng;

// ---------------------------------------------------------------------------
// Duplicate types — kept local for now
// ---------------------------------------------------------------------------
// PinState / PinDirection / BleState / PowerMode / BatteryState /
// HeartRateMeasurement / StepData / SpO2Measurement / SimulatedFlash /
// GpioController / BleController / TemperatureSensor / HeartRateSensor /
// SpO2Sensor / StepCounter / Accelerometer all have equivalent (but
// not byte-identical) definitions in `magent_hal::nrf52::sim`. A
// wholesale consolidation would require updating every consumer
// (SmartwatchSimulator, SmartwatchAgent, execute_tool, tests) and
// would need a careful diff against `magent_hal`'s API surface
// (notably the BatteryState atomic fields, StepData's `cadence` vs
// `activity` field, and the atomic vs plain-field storage of various
// sensor states). The next refactor pass should:
//
//  1. Build a field-by-field comparison table between this file and
//     `magent_hal/src/nrf52/sim.rs`. ← done, see docs/SIMULATOR_DEDUP_PLAN.md
//  2. Decide per type whether to:
//       (a) swap to the magent-hal type unchanged,
//       (b) keep the local type and only `use` the magent-hal trait
//           adapters in SmartwatchSimulator, or
//       (c) move the local type into magent-hal as a richer variant.
//  3. Rewrite SmartwatchSimulator to *wrap* magent_hal::Nrf52Simulator
//     (composition over duplication), keeping its own
//     VoiceProcessor / NetworkProcessor / SmartHomeController as
//     composed fields rather than duplicated sub-simulators.
//
// The full per-type comparison table and the migration plan live in
// `docs/SIMULATOR_DEDUP_PLAN.md`. Steps 1-7 in §7 of that document
// are mechanical and can be applied incrementally with `cargo check`
// between each step.
//
// The simple §1 swap (4 byte-identical enums) is blocked on a serde
// question: `HealthData` / `SystemInfo` derive `Serialize`/`Deserialize`
// and embed these enums. magent-hal currently has no `serde` dep —
// before §1 can land, magent-hal needs either a `serde` feature flag
// (preferred — small, surgical) or the host's serde derives need to
// be dropped (breaking — `pub` API change).

#[cfg(feature = "ollama")]
use std::sync::{Arc, Mutex};

// ============================================================================
// Constants
// ============================================================================

/// Flash size: 1MB
pub const FLASH_SIZE: usize = 1024 * 1024;

/// RAM size: 256KB
pub const RAM_SIZE: usize = 256 * 1024;

/// Number of GPIO pins
pub const GPIO_PIN_COUNT: usize = 48;

// ============================================================================
// Enums and Types
// ============================================================================

/// GPIO pin state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PinState { Low, High }

/// GPIO pin direction
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PinDirection { Input, Output, InputPullUp, InputPullDown }

/// BLE state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BleState { Disconnected, Advertising, Scanning, Connected }

/// Power mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerMode { Active, Idle, LowPower, SystemOff }

/// Agent state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState { Idle, Thinking, Acting, Observing, Finished, Error }

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentState::Idle => write!(f, "Idle"),
            AgentState::Thinking => write!(f, "Thinking"),
            AgentState::Acting => write!(f, "Acting"),
            AgentState::Observing => write!(f, "Observing"),
            AgentState::Finished => write!(f, "Finished"),
            AgentState::Error => write!(f, "Error"),
        }
    }
}

// ============================================================================
// Data Structures
// ============================================================================

/// Heart rate measurement
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HeartRateMeasurement {
    pub rate: u16,
    pub sensor_contact: bool,
    pub energy: u16,
}

/// Step data
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StepData {
    pub steps: u32,
    pub stride_length: u8,
    pub cadence: u16,
}

/// SpO2 measurement
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SpO2Measurement {
    pub saturation: f32,
    pub confidence: u8,
}

/// Battery state
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BatteryState {
    pub voltage_mv: u32,
    pub percentage: u32,
    pub charging: bool,
    pub low_battery: bool,
    pub health: u8,
}

impl Default for BatteryState {
    fn default() -> Self {
        Self {
            voltage_mv: 3700,
            percentage: 85,
            charging: false,
            low_battery: false,
            health: 100,
        }
    }
}

/// Health data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthData {
    pub heart_rate: HeartRateMeasurement,
    pub spo2: SpO2Measurement,
    pub steps: StepData,
    pub battery: BatteryState,
    pub temperature: f32,
    pub accelerometer: (f32, f32, f32),
}

/// System info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub flash_size: usize,
    pub ram_size: usize,
    pub gpio_pins: usize,
    pub uptime_seconds: u64,
    pub power_mode: PowerMode,
    pub ble_state: BleState,
}

// ============================================================================
// Voice Processing Types (STT/TTS)
// ============================================================================

/// Voice recognition result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeechRecognitionResult {
    pub text: String,
    pub confidence: f32,
    pub language: String,
}

/// Voice synthesis request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeechSynthesisRequest {
    pub text: String,
    pub language: String,
    pub speed: f32,
}

/// Voice state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    Idle,
    Listening,
    Processing,
    Speaking,
}

// ============================================================================
// Network Types (Web Search/Summarization)
// ============================================================================

/// Web search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Summarization result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryResult {
    pub summary: String,
    pub key_points: Vec<String>,
    pub word_count: usize,
}

// ============================================================================
// Smart Home Types (IoT Control)
// ============================================================================

/// Smart home device type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceType {
    Light,
    Thermostat,
    Lock,
    Camera,
    Speaker,
    Fan,
    AC,
    TV,
    Unknown,
}

/// Device state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceState {
    pub name: String,
    pub device_type: DeviceType,
    pub on: bool,
    pub brightness: Option<u8>,
    pub temperature: Option<f32>,
    pub locked: Option<bool>,
}

// ============================================================================
// Tool Types
// ============================================================================

/// Tool call
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub args: Vec<(String, String)>,
}

/// Tool result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub content: String,
    pub error: Option<String>,
}

impl ToolResult {
    pub fn ok(content: &str) -> Self {
        Self { success: true, content: content.to_string(), error: None }
    }
    pub fn err(error: &str) -> Self {
        Self { success: false, content: String::new(), error: Some(error.to_string()) }
    }
}

/// Message
#[derive(Debug, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn user(content: &str) -> Self { Self { role: "user".to_string(), content: content.to_string() } }
    pub fn assistant(content: &str) -> Self { Self { role: "assistant".to_string(), content: content.to_string() } }
    pub fn system(content: &str) -> Self { Self { role: "system".to_string(), content: content.to_string() } }
    pub fn tool(content: &str) -> Self { Self { role: "tool".to_string(), content: content.to_string() } }
}

/// Agent config
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub name: String,
    pub max_iterations: usize,
    pub max_tool_calls: usize,
    pub verbose: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "mAgent".to_string(),
            max_iterations: 20,
            max_tool_calls: 10,
            verbose: true,
        }
    }
}

// ============================================================================
// Simulated Hardware
// ============================================================================

/// Simulated flash storage
#[derive(Clone)]
pub struct SimulatedFlash {
    data: Vec<u8>,
    writes: Vec<u32>,
    sector_size: usize,
}

impl SimulatedFlash {
    pub fn new(size: usize) -> Self {
        let sector_size = 4096;
        Self {
            data: vec![0xFF; size],
            writes: vec![0u32; size / sector_size],
            sector_size,
        }
    }
    pub fn write(&mut self, addr: usize, data: &[u8]) -> Result<(), String> {
        if addr + data.len() > self.data.len() { return Err("Bad address".into()); }
        let sector = addr / self.sector_size;
        if sector < self.writes.len() { self.writes[sector] += 1; }
        for (i, &b) in data.iter().enumerate() { self.data[addr + i] &= b; }
        Ok(())
    }
    pub fn read(&self, addr: usize, buf: &mut [u8]) -> Result<(), String> {
        if addr + buf.len() > self.data.len() { return Err("Bad address".into()); }
        buf.copy_from_slice(&self.data[addr..addr + buf.len()]);
        Ok(())
    }
}

/// GPIO controller
#[derive(Clone)]
pub struct GpioController {
    pins: Vec<(PinDirection, PinState)>,
}

impl GpioController {
    pub fn new() -> Self {
        Self { pins: vec![(PinDirection::Input, PinState::Low); GPIO_PIN_COUNT] }
    }
    pub fn set_state(&mut self, pin: usize, state: PinState) -> Result<(), String> {
        if pin >= GPIO_PIN_COUNT { return Err(format!("Invalid pin: {}", pin)); }
        self.pins[pin].1 = state;
        Ok(())
    }
    pub fn get_state(&self, pin: usize) -> Result<PinState, String> {
        if pin >= GPIO_PIN_COUNT { return Err(format!("Invalid pin: {}", pin)); }
        Ok(self.pins[pin].1)
    }
    pub fn toggle(&mut self, pin: usize) -> Result<PinState, String> {
        if pin >= GPIO_PIN_COUNT { return Err(format!("Invalid pin: {}", pin)); }
        self.pins[pin].1 = match self.pins[pin].1 {
            PinState::Low => PinState::High,
            PinState::High => PinState::Low,
        };
        Ok(self.pins[pin].1)
    }
}

impl Default for GpioController {
    fn default() -> Self { Self::new() }
}

/// BLE controller
#[derive(Clone)]
pub struct BleController {
    pub state: BleState,
    pub connected_device: Option<String>,
    pub tx_count: usize,
    pub last_tx: Option<Vec<u8>>,
}

impl BleController {
    pub fn new() -> Self {
        Self { state: BleState::Disconnected, connected_device: None, tx_count: 0, last_tx: None }
    }
    pub fn connect(&mut self, device: &str) { self.state = BleState::Connected; self.connected_device = Some(device.into()); }
    pub fn disconnect(&mut self) { self.state = BleState::Disconnected; self.connected_device = None; }
    pub fn send(&mut self, data: &[u8]) -> Result<(), String> {
        if !matches!(self.state, BleState::Connected) { return Err("Not connected".into()); }
        self.tx_count += data.len();
        self.last_tx = Some(data.to_vec());
        Ok(())
    }
}

impl Default for BleController {
    fn default() -> Self { Self::new() }
}

/// Temperature sensor
#[derive(Clone)]
pub struct TemperatureSensor { base: f32, iter: u64 }

impl TemperatureSensor {
    pub fn new() -> Self { Self { base: 25.0, iter: 0 } }
    pub fn tick(&mut self) { self.iter += 1; }
    pub fn read(&self) -> f32 {
        let noise = (self.iter as f32 * 0.1).sin() * 0.5;
        self.base + noise
    }
}

impl Default for TemperatureSensor { fn default() -> Self { Self::new() } }

/// Heart rate sensor
#[derive(Clone)]
pub struct HeartRateSensor { rate: u16, iter: u64 }

impl HeartRateSensor {
    pub fn new() -> Self { Self { rate: 72, iter: 0 } }
    pub fn tick(&mut self) {
        self.iter += 1;
        let var = ((self.iter as f32 * 0.05).sin() * 5.0) as i16;
        self.rate = (70 + var).clamp(50, 180) as u16;
    }
    pub fn read(&self) -> HeartRateMeasurement { HeartRateMeasurement { rate: self.rate, sensor_contact: true, energy: 0 } }
}

impl Default for HeartRateSensor { fn default() -> Self { Self::new() } }

/// SpO2 sensor
#[derive(Clone)]
pub struct SpO2Sensor { sat: f32, iter: u64 }

impl SpO2Sensor {
    pub fn new() -> Self { Self { sat: 98.0, iter: 0 } }
    pub fn tick(&mut self) {
        self.iter += 1;
        let var = (self.iter as f32 * 0.02).sin() * 0.5;
        self.sat = (98.0 + var).clamp(90.0, 100.0);
    }
    pub fn read(&self) -> SpO2Measurement { SpO2Measurement { saturation: self.sat, confidence: 95 } }
}

impl Default for SpO2Sensor { fn default() -> Self { Self::new() } }

/// Step counter
#[derive(Clone)]
pub struct StepCounter { steps: u32 }

impl StepCounter {
    pub fn new() -> Self { Self { steps: 0 } }
    pub fn tick(&mut self, z: f32) {
        if z > 10.5 && self.steps.is_multiple_of(2) { self.steps += 1; }
    }
    pub fn read(&self) -> StepData { StepData { steps: self.steps, stride_length: 75, cadence: 100 } }
    pub fn add_steps(&mut self, n: u32) { self.steps += n; }
}

impl Default for StepCounter { fn default() -> Self { Self::new() } }

/// Accelerometer
#[derive(Clone)]
pub struct Accelerometer { x: f32, y: f32, z: f32, iter: u64 }

impl Accelerometer {
    pub fn new() -> Self { Self { x: 0.0, y: 0.0, z: 9.8, iter: 0 } }
    pub fn tick(&mut self) {
        self.iter += 1;
        let mut rng = rand::thread_rng();
        self.x = rng.gen_range(-0.1..0.1);
        self.y = rng.gen_range(-0.1..0.1);
        self.z = 9.8 + rng.gen_range(-0.2..0.2);
    }
    pub fn read(&self) -> (f32, f32, f32) { (self.x, self.y, self.z) }
    pub fn simulate_activity(&mut self, intensity: f32) {
        let mut rng = rand::thread_rng();
        self.x = rng.gen_range(-intensity..intensity);
        self.y = rng.gen_range(-intensity..intensity);
        self.z = 9.8 + rng.gen_range(-intensity..intensity);
    }
}

impl Default for Accelerometer { fn default() -> Self { Self::new() } }

// ============================================================================
// Voice Processing (STT/TTS)
// ============================================================================

/// Voice processor - simulates speech recognition and synthesis
#[derive(Clone)]
pub struct VoiceProcessor {
    state: VoiceState,
    language: String,
    stt_calls: usize,
    tts_calls: usize,
    last_transcript: Option<String>,
    last_speech: Option<String>,
}

impl VoiceProcessor {
    pub fn new() -> Self {
        Self {
            state: VoiceState::Idle,
            language: "en-US".to_string(),
            stt_calls: 0,
            tts_calls: 0,
            last_transcript: None,
            last_speech: None,
        }
    }

    /// Start listening for speech
    pub fn start_listening(&mut self) {
        self.state = VoiceState::Listening;
    }

    /// Stop listening and process audio
    pub fn stop_listening(&mut self) -> SpeechRecognitionResult {
        self.state = VoiceState::Processing;
        self.stt_calls += 1;

        // Simulate speech recognition with realistic results
        let mut rng = rand::thread_rng();
        let confidence = rng.gen_range(0.85..0.99);

        // Return simulated transcript based on language
        let transcript = match self.language.as_str() {
            "zh-CN" => "帮我查看心率和血氧".to_string(),
            "ja-JP" => "心拍数をチェック".to_string(),
            _ => "check my heart rate".to_string(),
        };

        self.last_transcript = Some(transcript.clone());
        self.state = VoiceState::Idle;

        SpeechRecognitionResult {
            text: transcript,
            confidence,
            language: self.language.clone(),
        }
    }

    /// Synthesize speech from text
    pub fn synthesize(&mut self, text: &str) -> String {
        self.state = VoiceState::Speaking;
        self.tts_calls += 1;
        self.last_speech = Some(text.to_string());

        // Simulate TTS processing
        let mut rng = rand::thread_rng();
        let duration_ms = (text.len() as u32 * 50) + rng.gen_range(0..500);

        self.state = VoiceState::Idle;
        format!("[TTS: {} chars, ~{}ms]", text.len(), duration_ms)
    }

    /// Cancel current operation
    pub fn cancel(&mut self) {
        self.state = VoiceState::Idle;
    }

    /// Get current state
    pub fn state(&self) -> VoiceState {
        self.state
    }

    /// Get statistics
    pub fn stats(&self) -> (usize, usize) {
        (self.stt_calls, self.tts_calls)
    }
}

impl Default for VoiceProcessor { fn default() -> Self { Self::new() } }

// ============================================================================
// Network Module (Web Search/Summarization)
// ============================================================================

/// Network processor - simulates web search and summarization
#[derive(Clone)]
pub struct NetworkProcessor {
    connected: bool,
    search_history: Vec<String>,
    summary_count: usize,
}

impl NetworkProcessor {
    pub fn new() -> Self {
        Self {
            connected: true, // Simulate connected state
            search_history: Vec::new(),
            summary_count: 0,
        }
    }

    /// Search the web
    pub fn search(&mut self, query: &str) -> Vec<SearchResult> {
        if !self.connected {
            return vec![];
        }

        self.search_history.push(query.to_string());

        // Generate simulated search results based on query
        let query_lower = query.to_lowercase();
        let results = if query_lower.contains("weather") {
            vec![
                SearchResult {
                    title: "Weather Forecast".to_string(),
                    url: "https://weather.example.com".to_string(),
                    snippet: "Partly cloudy, 22°C, humidity 65%".to_string(),
                },
                SearchResult {
                    title: "Local Weather".to_string(),
                    url: "https://localweather.example.com".to_string(),
                    snippet: "Today: Sunny, High 25°C, Low 18°C".to_string(),
                },
            ]
        } else if query_lower.contains("news") {
            vec![
                SearchResult {
                    title: "Latest News".to_string(),
                    url: "https://news.example.com".to_string(),
                    snippet: "Top headlines from around the world".to_string(),
                },
            ]
        } else if query_lower.contains("health") || query_lower.contains("heart") {
            vec![
                SearchResult {
                    title: "Heart Health Guide".to_string(),
                    url: "https://health.example.com/heart".to_string(),
                    snippet: "Normal resting heart rate: 60-100 bpm. Regular exercise helps maintain heart health.".to_string(),
                },
                SearchResult {
                    title: "SpO2 Information".to_string(),
                    url: "https://health.example.com/spo2".to_string(),
                    snippet: "Normal blood oxygen levels: 95-100%. Below 90% requires medical attention.".to_string(),
                },
            ]
        } else {
            vec![
                SearchResult {
                    title: format!("Results for: {}", query),
                    url: "https://search.example.com".to_string(),
                    snippet: "Relevant information about your query.".to_string(),
                },
            ]
        };

        results
    }

    /// Summarize text
    pub fn summarize(&mut self, text: &str) -> SummaryResult {
        self.summary_count += 1;

        // Simple summarization simulation
        let word_count = text.split_whitespace().count();
        let key_points = if text.to_lowercase().contains("temperature") {
            vec![
                "Current temperature reading".to_string(),
                "Temperature within normal range".to_string(),
            ]
        } else if text.to_lowercase().contains("heart") {
            vec![
                "Heart rate measured".to_string(),
                "Cardiovascular status normal".to_string(),
            ]
        } else {
            vec!["Key information extracted".to_string()]
        };

        SummaryResult {
            summary: format!("Summary: {} words processed into {} key points", word_count, key_points.len()),
            key_points,
            word_count,
        }
    }

    /// Check connection status
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Get search history
    pub fn search_history(&self) -> &[String] {
        &self.search_history
    }
}

impl Default for NetworkProcessor { fn default() -> Self { Self::new() } }

// ============================================================================
// Smart Home Controller (IoT)
// ============================================================================

/// Smart home controller - simulates IoT device control
#[derive(Clone)]
pub struct SmartHomeController {
    devices: Vec<DeviceState>,
}

impl SmartHomeController {
    pub fn new() -> Self {
        // Initialize with default devices
        let devices = vec![
            DeviceState {
                name: "Living Room Light".to_string(),
                device_type: DeviceType::Light,
                on: false,
                brightness: Some(100),
                temperature: None,
                locked: None,
            },
            DeviceState {
                name: "Bedroom Light".to_string(),
                device_type: DeviceType::Light,
                on: false,
                brightness: Some(80),
                temperature: None,
                locked: None,
            },
            DeviceState {
                name: "Smart Thermostat".to_string(),
                device_type: DeviceType::Thermostat,
                on: true,
                brightness: None,
                temperature: Some(22.0),
                locked: None,
            },
            DeviceState {
                name: "Front Door Lock".to_string(),
                device_type: DeviceType::Lock,
                on: false,
                brightness: None,
                temperature: None,
                locked: Some(true),
            },
            DeviceState {
                name: "Living Room Fan".to_string(),
                device_type: DeviceType::Fan,
                on: false,
                brightness: None,
                temperature: None,
                locked: None,
            },
            DeviceState {
                name: "Smart TV".to_string(),
                device_type: DeviceType::TV,
                on: false,
                brightness: None,
                temperature: None,
                locked: None,
            },
        ];

        Self { devices }
    }

    /// Turn device on
    pub fn turn_on(&mut self, device_name: &str) -> Result<String, String> {
        for device in &mut self.devices {
            if device.name.to_lowercase().contains(&device_name.to_lowercase()) {
                device.on = true;
                return Ok(format!("{} turned on", device.name));
            }
        }
        Err(format!("Device '{}' not found", device_name))
    }

    /// Turn device off
    pub fn turn_off(&mut self, device_name: &str) -> Result<String, String> {
        for device in &mut self.devices {
            if device.name.to_lowercase().contains(&device_name.to_lowercase()) {
                device.on = false;
                return Ok(format!("{} turned off", device.name));
            }
        }
        Err(format!("Device '{}' not found", device_name))
    }

    /// Set light brightness
    pub fn set_brightness(&mut self, device_name: &str, brightness: u8) -> Result<String, String> {
        for device in &mut self.devices {
            if device.name.to_lowercase().contains(&device_name.to_lowercase())
                && matches!(device.device_type, DeviceType::Light)
            {
                device.brightness = Some(brightness);
                return Ok(format!("{} brightness set to {}%", device.name, brightness));
            }
        }
        Err(format!("Light '{}' not found", device_name))
    }

    /// Set thermostat temperature
    pub fn set_temperature(&mut self, temperature: f32) -> Result<String, String> {
        for device in &mut self.devices {
            if matches!(device.device_type, DeviceType::Thermostat) {
                device.temperature = Some(temperature);
                return Ok(format!("Thermostat set to {:.1}°C", temperature));
            }
        }
        Err("Thermostat not found".to_string())
    }

    /// Lock/unlock door
    pub fn set_lock(&mut self, locked: bool) -> Result<String, String> {
        for device in &mut self.devices {
            if matches!(device.device_type, DeviceType::Lock) {
                device.locked = Some(locked);
                return Ok(format!("Door {}", if locked { "locked" } else { "unlocked" }));
            }
        }
        Err("Lock not found".to_string())
    }

    /// Get all devices
    pub fn get_devices(&self) -> &[DeviceState] {
        &self.devices
    }

    /// Get device by name
    pub fn get_device(&self, name: &str) -> Option<&DeviceState> {
        self.devices.iter().find(|d| d.name.to_lowercase().contains(&name.to_lowercase()))
    }

    /// Get all on devices
    pub fn get_active_devices(&self) -> Vec<&DeviceState> {
        self.devices.iter().filter(|d| d.on).collect()
    }
}

impl Default for SmartHomeController { fn default() -> Self { Self::new() } }

// ============================================================================
// Smartwatch Simulator
// ============================================================================

/// Complete smartwatch simulator
pub struct SmartwatchSimulator {
    pub flash: SimulatedFlash,
    pub gpio: GpioController,
    pub ble: BleController,
    pub temperature: TemperatureSensor,
    pub accelerometer: Accelerometer,
    pub heart_rate: HeartRateSensor,
    pub spo2: SpO2Sensor,
    pub steps: StepCounter,
    pub battery: BatteryState,
    pub power_mode: PowerMode,
    pub ticks: u64,
    pub rtc_seconds: u64,
    pub voice: VoiceProcessor,
    pub network: NetworkProcessor,
    pub smart_home: SmartHomeController,
}

impl SmartwatchSimulator {
    pub fn new() -> Self {
        let mut sim = Self {
            flash: SimulatedFlash::new(FLASH_SIZE),
            gpio: GpioController::new(),
            ble: BleController::new(),
            temperature: TemperatureSensor::new(),
            accelerometer: Accelerometer::new(),
            heart_rate: HeartRateSensor::new(),
            spo2: SpO2Sensor::new(),
            steps: StepCounter::new(),
            battery: BatteryState::default(),
            power_mode: PowerMode::Active,
            ticks: 0,
            rtc_seconds: 0,
            voice: VoiceProcessor::new(),
            network: NetworkProcessor::new(),
            smart_home: SmartHomeController::new(),
        };
        // Auto-connect BLE for simulation
        sim.ble.connect("Simulator");
        sim
    }

    pub fn tick(&mut self) {
        self.ticks += 1;
        if self.ticks.is_multiple_of(1000) { self.rtc_seconds += 1; }
        self.temperature.tick();
        self.accelerometer.tick();
        self.heart_rate.tick();
        self.spo2.tick();
        self.steps.tick(self.accelerometer.read().2);
    }

    pub fn execute_tool(&mut self, tool: &str, args: &[(&str, &str)]) -> Result<String, String> {
        match tool {
            // === Existing Sensor Tools ===
            "read_sensor" => {
                let sensor = args.iter().find(|(k, _)| *k == "sensor").map(|(_, v)| *v).unwrap_or("temperature");
                match sensor {
                    "temperature" => Ok(format!("{:.1}", self.temperature.read())),
                    "heart_rate" | "hr" => Ok(format!("{}", self.heart_rate.read().rate)),
                    "spo2" => Ok(format!("{:.1}", self.spo2.read().saturation)),
                    "accelerometer" | "accel" => {
                        let (x, y, z) = self.accelerometer.read();
                        Ok(format!("X={:.2} Y={:.2} Z={:.2}", x, y, z))
                    }
                    "steps" => Ok(format!("{}", self.steps.read().steps)),
                    "humidity" => Ok("55.0".to_string()),
                    "light" => Ok("500.0".to_string()),
                    _ => Err(format!("Unknown sensor: {}", sensor)),
                }
            }

            // === GPIO Tools ===
            "write_gpio" => {
                let pin = args.iter().find(|(k, _)| *k == "pin").and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(13);
                let state = args.iter().find(|(k, _)| *k == "state").map(|(_, v)| *v).unwrap_or("high");
                let ps = if state == "high" { PinState::High } else { PinState::Low };
                self.gpio.set_state(pin, ps)?;
                Ok(format!("GPIO {} set to {}", pin, state))
            }
            "read_gpio" => {
                let pin = args.iter().find(|(k, _)| *k == "pin").and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(13);
                let state = self.gpio.get_state(pin)?;
                Ok(format!("{:?}", state))
            }

            // === Flash Storage Tools ===
            "flash_write" => {
                let addr = args.iter().find(|(k, _)| *k == "address").and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0);
                let data = args.iter().find(|(k, _)| *k == "data").map(|(_, v)| *v).unwrap_or("");
                self.flash.write(addr, data.as_bytes())?;
                Ok(format!("Wrote {} bytes to flash at 0x{:04X}", data.len(), addr))
            }
            "flash_read" => {
                let addr = args.iter().find(|(k, _)| *k == "address").and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0);
                let mut buf = [0u8; 64];
                self.flash.read(addr, &mut buf)?;
                let hex: String = buf.iter().take(16).map(|b| format!("{:02X}", b)).collect();
                Ok(format!("Read at 0x{:04X}: {}", addr, hex))
            }

            // === BLE Tools ===
            "ble_send" => {
                let data = args.iter().find(|(k, _)| *k == "data").map(|(_, v)| *v).unwrap_or("");
                self.ble.send(data.as_bytes())?;
                Ok(format!("Sent via BLE: {}", data))
            }

            // === System Tools ===
            "get_battery" => Ok(format!("{}% ({}mV)", self.battery.percentage, self.battery.voltage_mv)),
            "get_status" => {
                let steps = self.steps.read().steps;
                Ok(format!("Battery: {}%, Steps: {}, BLE: {:?}", self.battery.percentage, steps, self.ble.state))
            }

            // === Voice Tools (NEW) ===
            "stt_start" => {
                self.voice.start_listening();
                Ok("Voice recognition started - listening...".to_string())
            }
            "stt_stop" => {
                let result = self.voice.stop_listening();
                Ok(format!("Recognized: '{}' (confidence: {:.0}%)", result.text, result.confidence * 100.0))
            }
            "tts_speak" => {
                let text = args.iter().find(|(k, _)| *k == "text").map(|(_, v)| *v).unwrap_or("Hello");
                let result = self.voice.synthesize(text);
                Ok(format!("Speaking: {} - {}", text, result))
            }
            "voice_status" => {
                let (stt, tts) = self.voice.stats();
                Ok(format!("Voice: {:?}, STT calls: {}, TTS calls: {}", self.voice.state(), stt, tts))
            }

            // === Network Tools (NEW) ===
            "web_search" => {
                let query = args.iter().find(|(k, _)| *k == "query").map(|(_, v)| *v).unwrap_or("");
                let results = self.network.search(query);
                if results.is_empty() {
                    return Err("No network connection".to_string());
                }
                let mut output = format!("Found {} results:\n", results.len());
                for (i, r) in results.iter().enumerate() {
                    output.push_str(&format!("{}. {} - {}\n   {}\n", i + 1, r.title, r.url, r.snippet));
                }
                Ok(output)
            }
            "summarize" => {
                let text = args.iter().find(|(k, _)| *k == "text").map(|(_, v)| *v).unwrap_or("");
                let result = self.network.summarize(text);
                Ok(format!("{}\nKey points: {:?}", result.summary, result.key_points))
            }
            "network_status" => {
                let status = if self.network.is_connected() { "Connected" } else { "Disconnected" };
                Ok(format!("Network: {}, Searches: {}", status, self.network.search_history.len()))
            }

            // === Smart Home Tools (NEW) ===
            "smarthome_on" => {
                let device = args.iter().find(|(k, _)| *k == "device").map(|(_, v)| *v).unwrap_or("light");
                self.smart_home.turn_on(device)
            }
            "smarthome_off" => {
                let device = args.iter().find(|(k, _)| *k == "device").map(|(_, v)| *v).unwrap_or("light");
                self.smart_home.turn_off(device)
            }
            "smarthome_brightness" => {
                let device = args.iter().find(|(k, _)| *k == "device").map(|(_, v)| *v).unwrap_or("light");
                let level = args.iter().find(|(k, _)| *k == "level").and_then(|(_, v)| v.parse::<u8>().ok()).unwrap_or(100);
                self.smart_home.set_brightness(device, level)
            }
            "smarthome_temperature" => {
                let temp = args.iter().find(|(k, _)| *k == "temperature").and_then(|(_, v)| v.parse::<f32>().ok()).unwrap_or(22.0);
                self.smart_home.set_temperature(temp)
            }
            "smarthome_lock" => {
                let locked = args.iter().find(|(k, _)| *k == "locked").map(|(_, v)| *v == "true").unwrap_or(true);
                self.smart_home.set_lock(locked)
            }
            "smarthome_list" => {
                let devices = self.smart_home.get_devices();
                let mut output = "Smart Home Devices:\n".to_string();
                for d in devices {
                    let status = if d.on { "ON" } else { "OFF" };
                    let extra = match d.device_type {
                        DeviceType::Light => format!(", brightness: {}%", d.brightness.unwrap_or(0)),
                        DeviceType::Thermostat => format!(", temp: {:.1}°C", d.temperature.unwrap_or(0.0)),
                        DeviceType::Lock => format!(", locked: {}", if d.locked.unwrap_or(false) { "yes" } else { "no" }),
                        _ => String::new(),
                    };
                    output.push_str(&format!("- {} [{}]{}\n", d.name, status, extra));
                }
                Ok(output)
            }

            _ => Err(format!("Unknown tool: {}", tool)),
        }
    }

    pub fn read_health_data(&self) -> HealthData {
        HealthData {
            heart_rate: self.heart_rate.read(),
            spo2: self.spo2.read(),
            steps: self.steps.read(),
            battery: self.battery,
            temperature: self.temperature.read(),
            accelerometer: self.accelerometer.read(),
        }
    }

    pub fn get_system_info(&self) -> SystemInfo {
        SystemInfo {
            flash_size: FLASH_SIZE,
            ram_size: RAM_SIZE,
            gpio_pins: GPIO_PIN_COUNT,
            uptime_seconds: self.rtc_seconds,
            power_mode: self.power_mode,
            ble_state: self.ble.state,
        }
    }
}

impl Default for SmartwatchSimulator { fn default() -> Self { Self::new() } }

// ============================================================================
// AI Agent
// ============================================================================

/// Smartwatch AI Agent
pub struct SmartwatchAgent {
    config: AgentConfig,
    state: AgentState,
    sim: SmartwatchSimulator,
    messages: Vec<Message>,
    tool_calls: usize,
    iteration: usize,
}

impl SmartwatchAgent {
    pub fn new(config: AgentConfig) -> Self {
        Self { config, state: AgentState::Idle, sim: SmartwatchSimulator::new(), messages: Vec::new(), tool_calls: 0, iteration: 0 }
    }

    pub fn with_defaults() -> Self { Self::new(AgentConfig::default()) }

    pub fn run(&mut self, task: &str) -> Result<String, String> {
        self.reset();
        if self.config.verbose {
            println!("\n{}", "=".repeat(60));
            println!("  {} - Smartwatch AI Agent", self.config.name);
            println!("{}", "=".repeat(60));
            println!("\n[TASK] {}", task);
        }
        self.messages.push(Message::user(task));
        self.state = AgentState::Thinking;

        while self.state != AgentState::Finished && self.state != AgentState::Error {
            if self.iteration >= self.config.max_iterations { break; }
            if self.tool_calls >= self.config.max_tool_calls { break; }
            self.iteration += 1;
            match self.state {
                AgentState::Thinking => self.think()?,
                AgentState::Acting => { self.tool_calls += 1; self.state = AgentState::Observing; }
                AgentState::Observing => self.state = AgentState::Thinking,
                _ => break,
            }
        }
        self.get_result()
    }

    fn reset(&mut self) {
        self.state = AgentState::Idle;
        self.messages.clear();
        self.tool_calls = 0;
        self.iteration = 0;
        self.sim = SmartwatchSimulator::new();
    }

    fn think(&mut self) -> Result<(), String> {
        if self.config.verbose { println!("\n[THINK] Iteration {}", self.iteration); }
        let health = self.sim.read_health_data();
        let context = format!("Battery: {}%, Steps: {}, HR: {} bpm, SpO2: {}%", health.battery.percentage, health.steps.steps, health.heart_rate.rate, health.spo2.saturation);
        let response = self.generate_response(&context);
        if self.config.verbose { println!("[REASONING] {}", response); }
        self.messages.push(Message::assistant(&response));
        if let Some(tc) = self.parse_tool_call(&response) {
            self.messages.push(Message::system(&format!("Executing: {:?}", tc)));
            self.execute_tool(&tc)?;
            self.state = AgentState::Acting;
        } else if response.contains("DONE") || response.contains("RESULT") {
            self.state = AgentState::Finished;
        } else {
            self.state = AgentState::Observing;
        }
        Ok(())
    }

    fn execute_tool(&mut self, tc: &ToolCall) -> Result<(), String> {
        let args: Vec<(&str, &str)> = tc.args.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        match self.sim.execute_tool(&tc.name, &args) {
            Ok(result) => {
                if self.config.verbose { println!("[RESULT] {}", result); }
                self.messages.push(Message::tool(&result));
                Ok(())
            }
            Err(e) => {
                let msg = format!("Error: {}", e);
                if self.config.verbose { println!("[ERROR] {}", msg); }
                self.messages.push(Message::tool(&msg));
                Err(e)
            }
        }
    }

    fn generate_response(&self, context: &str) -> String {
        let task = self.messages.iter().find(|m| m.role == "user").map(|m| m.content.to_lowercase()).unwrap_or_default();
        let tool_count = self.messages.iter().filter(|m| m.role == "tool").count();

        // === Voice Tools ===
        if task.contains("voice") || task.contains("speech") || task.contains("listen") || task.contains("microphone") || task.contains("语音") {
            if task.contains("start") || task.contains("listen") || task.contains("开始") {
                return r#"{"action": "stt_start"}"#.to_string();
            }
            if task.contains("stop") || task.contains("recognize") || task.contains("停止") {
                return r#"{"action": "stt_stop"}"#.to_string();
            }
            if task.contains("speak") || task.contains("say") || task.contains("tell") || task.contains("说话") || task.contains("播报") {
                return r#"{"action": "tts_speak", "args": {"text": "Your health metrics look great!"}}"#.to_string();
            }
        }

        // === Network Tools ===
        if task.contains("search") || task.contains("web") || task.contains("internet") || task.contains("查询") || task.contains("搜索") {
            if tool_count == 0 {
                return r#"{"action": "web_search", "args": {"query": "weather today"}}"#.to_string();
            }
            return r#"{"done": true, "result": "Web search completed"}"#.to_string();
        }

        if task.contains("summarize") || task.contains("summary") || task.contains("总结") {
            if tool_count == 0 {
                return r#"{"action": "summarize", "args": {"text": "Long health report text to summarize"}}"#.to_string();
            }
            return r#"{"done": true, "result": "Text summarized successfully"}"#.to_string();
        }

        // === Smart Home Tools ===
        if task.contains("turn on") || task.contains("enable") || task.contains("开灯") || task.contains("打开") {
            if task.contains("light") || task.contains("lamp") || task.contains("灯") {
                return r#"{"action": "smarthome_on", "args": {"device": "light"}}"#.to_string();
            }
            if task.contains("fan") || task.contains("风扇") {
                return r#"{"action": "smarthome_on", "args": {"device": "fan"}}"#.to_string();
            }
            if task.contains("tv") || task.contains("television") || task.contains("电视") {
                return r#"{"action": "smarthome_on", "args": {"device": "tv"}}"#.to_string();
            }
        }

        if task.contains("turn off") || task.contains("disable") || task.contains("关灯") || task.contains("关闭") {
            if task.contains("light") || task.contains("lamp") || task.contains("灯") {
                return r#"{"action": "smarthome_off", "args": {"device": "light"}}"#.to_string();
            }
            if task.contains("fan") || task.contains("风扇") {
                return r#"{"action": "smarthome_off", "args": {"device": "fan"}}"#.to_string();
            }
            if task.contains("tv") || task.contains("television") || task.contains("电视") {
                return r#"{"action": "smarthome_off", "args": {"device": "tv"}}"#.to_string();
            }
        }

        if task.contains("brightness") || task.contains("dim") || task.contains("调暗") || task.contains("调亮") {
            let level = if task.contains("dim") || task.contains("暗") { 30 } else if task.contains("bright") || task.contains("亮") { 100 } else { 70 };
            return format!(r#"{{"action": "smarthome_brightness", "args": {{"device": "light", "level": {}}}}}"#, level);
        }

        if task.contains("temperature") && (task.contains("set") || task.contains("thermostat") || task.contains("温度") || task.contains("空调")) {
            return r#"{"action": "smarthome_temperature", "args": {"temperature": "22"}}"#.to_string();
        }

        if task.contains("lock") || task.contains("unlock") || task.contains("门") || task.contains("锁") {
            let locked = !task.contains("unlock") && !task.contains("开");
            return format!(r#"{{"action": "smarthome_lock", "args": {{"locked": "{}"}}}}"#, locked);
        }

        if task.contains("devices") || task.contains("smarthome") || task.contains("home") || task.contains("智能家居") || task.contains("设备") {
            if tool_count == 0 {
                return r#"{"action": "smarthome_list"}"#.to_string();
            }
            return format!(r#"{{"done": true, "result": "Smart home devices updated. {}"}}"#, context);
        }

        // === Existing Health/Sensor Tools ===
        // Health monitoring
        if task.contains("health") || task.contains("vital") || task.contains("monitor") {
            match tool_count {
                0 => return r#"{"action": "read_sensor", "args": {"sensor": "heart_rate"}}"#.to_string(),
                1 => return r#"{"action": "read_sensor", "args": {"sensor": "spo2"}}"#.to_string(),
                2 => return r#"{"action": "get_battery"}"#.to_string(),
                3 => return r#"{"action": "ble_send", "args": {"data": "Health report ready"}}"#.to_string(),
                _ => return format!(r#"{{"done": true, "result": "Health check complete. {}"}}"#, context),
            }
        }

        // Temperature
        if task.contains("temperature") || task.contains("temp") {
            if tool_count == 0 { return r#"{"action": "read_sensor", "args": {"sensor": "temperature"}}"#.to_string(); }
            return format!(r#"{{"done": true, "result": "Temperature read. {}"}}"#, context);
        }

        // LED control
        if task.contains("led") {
            if task.contains("on") || task.contains("enable") { return r#"{"action": "write_gpio", "args": {"pin": "13", "state": "high"}}"#.to_string(); }
            if task.contains("off") || task.contains("disable") { return r#"{"action": "write_gpio", "args": {"pin": "13", "state": "low"}}"#.to_string(); }
        }

        // BLE notification
        if task.contains("ble") || task.contains("notify") || task.contains("send") || task.contains("alert") {
            if tool_count == 0 { return r#"{"action": "ble_send", "args": {"data": "Notification from mAgent"}}"#.to_string(); }
            return r#"{"done": true, "result": "BLE notification sent"}"#.to_string();
        }

        // Flash storage
        if task.contains("flash") || task.contains("log") || task.contains("save") || task.contains("store") {
            if tool_count == 0 { return r#"{"action": "read_sensor", "args": {"sensor": "steps"}}"#.to_string(); }
            if tool_count == 1 { return r#"{"action": "flash_write", "args": {"address": "0", "data": "Activity log"}}"#.to_string(); }
            return r#"{"done": true, "result": "Data logged to flash"}"#.to_string();
        }

        // Steps/activity
        if task.contains("step") || task.contains("activity") || task.contains("exercise") {
            if tool_count == 0 { return r#"{"action": "read_sensor", "args": {"sensor": "steps"}}"#.to_string(); }
            return format!(r#"{{"done": true, "result": "Activity tracked. {}"}}"#, context);
        }

        // Status check
        if task.contains("status") || task.contains("system") || task.contains("check") {
            if tool_count == 0 { return r#"{"action": "get_status"}"#.to_string(); }
            return format!(r#"{{"done": true, "result": "System status: {}"}}"#, context);
        }

        // Default: battery
        if tool_count == 0 { return r#"{"action": "get_battery"}"#.to_string(); }
        format!(r#"{{"done": true, "result": "Task complete. {}"}}"#, context)
    }

    fn parse_tool_call(&self, response: &str) -> Option<ToolCall> {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(response) {
            if let Some(action) = json.get("action").or(json.get("tool")).and_then(|v| v.as_str()) {
                let mut args = Vec::new();
                if let Some(sensor) = json.get("sensor").and_then(|v| v.as_str()) { args.push(("sensor".into(), sensor.into())); }
                if let Some(pin_val) = json.get("pin") {
                    let pin_str = if let Some(s) = pin_val.as_str() {
                        s.to_string()
                    } else if let Some(n) = pin_val.as_i64() {
                        n.to_string()
                    } else {
                        String::new()
                    };
                    args.push(("pin".into(), pin_str));
                }
                if let Some(state) = json.get("state").and_then(|v| v.as_str()) { args.push(("state".into(), state.into())); }
                if let Some(data) = json.get("data").and_then(|v| v.as_str()) { args.push(("data".into(), data.into())); }
                if let Some(addr_val) = json.get("address") {
                    let addr_str = if let Some(s) = addr_val.as_str() {
                        s.to_string()
                    } else if let Some(n) = addr_val.as_i64() {
                        n.to_string()
                    } else {
                        String::new()
                    };
                    args.push(("address".into(), addr_str));
                }
                return Some(ToolCall { name: action.into(), args });
            }
        }
        None
    }

    fn get_result(&self) -> Result<String, String> {
        for msg in self.messages.iter().rev() {
            if msg.role == "assistant" {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                    if let Some(r) = json.get("result").and_then(|v| v.as_str()) { return Ok(r.to_string()); }
                }
            }
        }
        for msg in self.messages.iter().rev() { if msg.role == "assistant" { return Ok(msg.content.clone()); } }
        Ok("Task completed".to_string())
    }

    pub fn state(&self) -> AgentState { self.state }
    pub fn iteration(&self) -> usize { self.iteration }
    pub fn tool_calls(&self) -> usize { self.tool_calls }
    pub fn health_data(&self) -> HealthData { self.sim.read_health_data() }
    pub fn system_info(&self) -> SystemInfo { self.sim.get_system_info() }
}

/// Run demo scenarios
pub fn run_demos() {
    println!("\n{}", "#".repeat(60));
    println!("  nRF52840 Smartwatch AI Agent - Extended Demo Scenarios");
    println!("  Version 2.0 - Voice, Network, Smart Home Support");
    println!("{}", "#".repeat(60));

    let mut agent = SmartwatchAgent::with_defaults();

    // Demo scenarios - including new features
    let scenarios = vec![
        // Original demos
        ("Health Monitoring", "Check my health: heart rate, SpO2, and send a report"),
        ("Temperature Check", "Read the temperature sensor"),
        ("LED Control", "Turn on the LED notification light"),
        ("Activity Tracking", "Track my steps and log to flash"),
        ("BLE Alert", "Send a reminder notification via BLE"),
        ("System Status", "Get system status report"),
        // New voice demos
        ("Voice Recognition", "Start voice recognition and listen to my command"),
        ("Text to Speech", "Speak out my health report using TTS"),
        // New network demos
        ("Web Search", "Search for today's weather"),
        ("Summarize Text", "Summarize this health report"),
        // New smart home demos
        ("Smart Home - Lights", "Turn on the living room light"),
        ("Smart Home - Thermostat", "Set the thermostat to 22 degrees"),
        ("Smart Home - Lock", "Lock the front door"),
        ("Smart Home - Devices", "Show all my smart home devices"),
    ];

    for (name, task) in scenarios {
        println!("\n\n{}", "-".repeat(60));
        println!("  Demo: {}", name);
        println!("{}", "-".repeat(60));
        match agent.run(task) {
            Ok(result) => println!("\n[RESULT] {}", result),
            Err(e) => println!("\n[ERROR] {}", e),
        }
    }

    // Final status
    println!("\n\n{}", "=".repeat(60));
    println!("  Final System Status");
    println!("{}", "=".repeat(60));
    let health = agent.health_data();
    let info = agent.system_info();
    println!("\n  Battery: {}% ({}mV)", health.battery.percentage, health.battery.voltage_mv);
    println!("  Steps: {}", health.steps.steps);
    println!("  Heart Rate: {} bpm", health.heart_rate.rate);
    println!("  SpO2: {:.1}%", health.spo2.saturation);
    println!("  Temperature: {:.1}°C", health.temperature);
    println!("  Uptime: {} seconds", info.uptime_seconds);
    println!("  BLE: {:?}", info.ble_state);
    println!("  Iterations: {}", agent.iteration());
    println!("  Tool Calls: {}", agent.tool_calls());
    println!("\n{}", "=".repeat(60));
    println!("  Available Tools:");
    println!("{}", "=".repeat(60));
    println!("  [Sensors] read_sensor, get_battery, get_status");
    println!("  [GPIO] write_gpio, read_gpio");
    println!("  [Storage] flash_write, flash_read");
    println!("  [BLE] ble_send");
    println!("  [Voice] stt_start, stt_stop, tts_speak, voice_status");
    println!("  [Network] web_search, summarize, network_status");
    println!("  [Smart Home] smarthome_on, smarthome_off, smarthome_brightness");
    println!("  [Smart Home] smarthome_temperature, smarthome_lock, smarthome_list");
    println!("\n{}", "=".repeat(60));
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simulator_creation() {
        let sim = SmartwatchSimulator::new();
        assert!(matches!(sim.power_mode, PowerMode::Active));
    }

    #[test]
    fn test_temperature_sensor() {
        let sim = SmartwatchSimulator::new();
        let temp = sim.temperature.read();
        assert!(temp > 20.0 && temp < 30.0);
    }

    #[test]
    fn test_heart_rate_sensor() {
        let mut sim = SmartwatchSimulator::new();
        for _ in 0..100 { sim.tick(); }
        let hr = sim.heart_rate.read();
        assert!(hr.rate >= 50 && hr.rate <= 180);
    }

    #[test]
    fn test_gpio_operations() {
        let mut sim = SmartwatchSimulator::new();
        sim.gpio.set_state(13, PinState::High).unwrap();
        assert_eq!(sim.gpio.get_state(13).unwrap(), PinState::High);
    }

    #[test]
    fn test_ble_connection() {
        let mut sim = SmartwatchSimulator::new();
        sim.ble.connect("Phone");
        assert!(matches!(sim.ble.state, BleState::Connected));
    }

    #[test]
    fn test_flash_write_read() {
        let mut sim = SmartwatchSimulator::new();
        sim.flash.write(0, b"Test").unwrap();
        let mut buf = [0u8; 4];
        sim.flash.read(0, &mut buf).unwrap();
        assert_eq!(&buf, b"Test");
    }

    #[test]
    fn test_tool_execution() {
        let mut sim = SmartwatchSimulator::new();
        let result = sim.execute_tool("read_sensor", &[("sensor", "temperature")]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_agent_creation() {
        let agent = SmartwatchAgent::with_defaults();
        assert_eq!(agent.state(), AgentState::Idle);
    }

    #[test]
    fn test_health_task() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Check my heart rate");
        assert!(result.is_ok());
    }

    #[test]
    fn test_temperature_task() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Read temperature");
        assert!(result.is_ok());
    }

    #[test]
    fn test_led_control() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Turn on the LED");
        assert!(result.is_ok());
    }

    #[test]
    fn test_ble_notification() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Send notification via BLE");
        assert!(result.is_ok());
    }

    #[test]
    fn test_flash_storage() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Log data to flash");
        assert!(result.is_ok());
    }

    #[test]
    fn test_activity_tracking() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Track my steps");
        assert!(result.is_ok());
    }

    #[test]
    fn test_iteration_tracking() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let _ = agent.run("Read temperature");
        assert!(agent.iteration() > 0);
    }

    #[test]
    fn test_tool_call_tracking() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let _ = agent.run("Read temperature");
        assert!(agent.tool_calls() > 0);
    }

    #[test]
    fn test_health_data() {
        let agent = SmartwatchAgent::with_defaults();
        let health = agent.health_data();
        assert!(health.heart_rate.rate >= 50);
        assert!(health.spo2.saturation >= 90.0);
    }

    #[test]
    fn test_system_info() {
        let agent = SmartwatchAgent::with_defaults();
        let info = agent.system_info();
        assert_eq!(info.flash_size, FLASH_SIZE);
        assert_eq!(info.ram_size, RAM_SIZE);
        assert_eq!(info.gpio_pins, GPIO_PIN_COUNT);
    }

    // === Voice Processing Tests ===
    #[test]
    fn test_voice_processor_creation() {
        let voice = VoiceProcessor::new();
        assert!(matches!(voice.state(), VoiceState::Idle));
    }

    #[test]
    fn test_stt_listening() {
        let mut voice = VoiceProcessor::new();
        voice.start_listening();
        assert!(matches!(voice.state(), VoiceState::Listening));

        let result = voice.stop_listening();
        assert!(!result.text.is_empty());
        assert!(result.confidence > 0.8);
    }

    #[test]
    fn test_tts_synthesis() {
        let mut voice = VoiceProcessor::new();
        let result = voice.synthesize("Hello world");
        assert!(result.contains("TTS"));
    }

    // === Network Processing Tests ===
    #[test]
    fn test_network_search() {
        let mut network = NetworkProcessor::new();
        let results = network.search("weather");
        assert!(!results.is_empty());
        assert_eq!(results[0].title, "Weather Forecast");
    }

    #[test]
    fn test_network_summarize() {
        let mut network = NetworkProcessor::new();
        let result = network.summarize("This is a test text to summarize");
        assert!(!result.summary.is_empty());
        assert!(result.word_count > 0);
    }

    #[test]
    fn test_network_connection() {
        let network = NetworkProcessor::new();
        assert!(network.is_connected());
    }

    // === Smart Home Tests ===
    #[test]
    fn test_smart_home_devices() {
        let home = SmartHomeController::new();
        let devices = home.get_devices();
        assert!(!devices.is_empty());
    }

    #[test]
    fn test_turn_on_light() {
        let mut home = SmartHomeController::new();
        let result = home.turn_on("living room light");
        assert!(result.is_ok());
        assert!(result.unwrap().contains("on"));
    }

    #[test]
    fn test_turn_off_device() {
        let mut home = SmartHomeController::new();
        home.turn_on("fan").unwrap();
        let result = home.turn_off("fan");
        assert!(result.is_ok());
    }

    #[test]
    fn test_set_brightness() {
        let mut home = SmartHomeController::new();
        let result = home.set_brightness("bedroom light", 50);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("50%"));
    }

    #[test]
    fn test_set_thermostat() {
        let mut home = SmartHomeController::new();
        let result = home.set_temperature(24.5);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("24.5"));
    }

    #[test]
    fn test_lock_door() {
        let mut home = SmartHomeController::new();
        let result = home.set_lock(true);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("locked"));
    }

    #[test]
    fn test_unlock_door() {
        let mut home = SmartHomeController::new();
        let result = home.set_lock(false);
        assert!(result.is_ok());
        assert!(result.unwrap().contains("unlocked"));
    }

    // === Voice-enabled Agent Tests ===
    #[test]
    fn test_voice_recognition_task() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Start voice recognition");
        assert!(result.is_ok());
    }

    #[test]
    fn test_tts_task() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Speak out my health report");
        assert!(result.is_ok());
    }

    // === Network-enabled Agent Tests ===
    #[test]
    fn test_web_search_task() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Search for weather information");
        assert!(result.is_ok());
    }

    #[test]
    fn test_summarize_task() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Summarize this health report");
        assert!(result.is_ok());
    }

    // === Smart Home Agent Tests ===
    #[test]
    fn test_smart_home_light_task() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Turn on the living room light");
        assert!(result.is_ok());
    }

    #[test]
    fn test_smart_home_thermostat_task() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Set thermostat to 22 degrees");
        assert!(result.is_ok());
    }

    #[test]
    fn test_smart_home_lock_task() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Lock the front door");
        assert!(result.is_ok());
    }

    #[test]
    fn test_smart_home_devices_task() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;
        let result = agent.run("Show all my smart home devices");
        assert!(result.is_ok());
    }
}

// ============================================================================
// Ollama LLM Integration (Optional)
// ============================================================================

#[cfg(feature = "ollama")]
pub mod ollama_integration {

    /// Ollama client for real LLM reasoning
    pub struct OllamaClient {
        base_url: String,
        model: String,
        client: reqwest::blocking::Client,
    }

    impl OllamaClient {
        /// Create new Ollama client
        pub fn new(base_url: &str, model: &str) -> Self {
            Self {
                base_url: base_url.to_string(),
                model: model.to_string(),
                client: reqwest::blocking::Client::builder()
                    .timeout(std::time::Duration::from_secs(120))
                    .build()
                    .unwrap_or_else(|_| reqwest::blocking::Client::new()),
            }
        }

        /// Check connection to Ollama
        pub fn check_connection(&self) -> bool {
            self.client
                .get(format!("{}/api/tags", self.base_url))
                .send()
                .map(|r| r.status().is_success())
                .unwrap_or(false)
        }

        /// Get available models
        pub fn get_models(&self) -> Vec<String> {
            if let Ok(response) = self.client.get(format!("{}/api/tags", self.base_url)).send() {
                if let Ok(json) = response.json::<serde_json::Value>() {
                    return json["models"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|m| m["name"].as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                }
            }
            vec![]
        }

        /// Chat completion with tool-aware prompt
        pub fn chat(&self, messages: &[(&str, &str)], system_prompt: &str) -> Result<String, String> {
            let mut chat_messages: Vec<serde_json::Value> = vec![serde_json::json!({
                "role": "system",
                "content": system_prompt
            })];

            for (role, content) in messages {
                chat_messages.push(serde_json::json!({
                    "role": role,
                    "content": content
                }));
            }

            let request_body = serde_json::json!({
                "model": self.model,
                "messages": chat_messages,
                "stream": false,
                "options": {
                    "temperature": 0.3,
                    "num_predict": 512
                }
            });

            let response = self
                .client
                .post(format!("{}/api/chat", self.base_url))
                .json(&request_body)
                .send()
                .map_err(|e| format!("Request failed: {}", e))?;

            if !response.status().is_success() {
                return Err(format!("HTTP error: {}", response.status()));
            }

            let json: serde_json::Value = response.json().map_err(|e| format!("JSON parse error: {}", e))?;
            json["message"]["content"]
                .as_str()
                .map(String::from)
                .ok_or_else(|| "No content in response".to_string())
        }
    }

    /// Agent with Ollama integration
    pub struct OllamaSmartwatchAgent {
        agent: crate::SmartwatchAgent,
        ollama: Option<OllamaClient>,
    }

    impl OllamaSmartwatchAgent {
        /// Create new agent with optional Ollama
        pub fn new(use_ollama: bool) -> Self {
            let ollama = if use_ollama {
                let client = OllamaClient::new("http://localhost:11434", "llama3:latest");
                if client.check_connection() {
                    Some(client)
                } else {
                    println!("Warning: Ollama not available, using simulated responses");
                    None
                }
            } else {
                None
            };

            Self {
                agent: crate::SmartwatchAgent::with_defaults(),
                ollama,
            }
        }

    /// Run task with Ollama if available
    pub fn run(&mut self, task: &str) -> Result<String, String> {
        // Take ollama out to avoid borrow conflict
        let ollama_client = self.ollama.take();
        let result = if let Some(ref client) = ollama_client {
            self.run_with_ollama(task, client)
        } else {
            self.agent.run(task)
        };
        // Put ollama back
        self.ollama = ollama_client;
        result
    }

        fn run_with_ollama(&mut self, task: &str, ollama_client: &OllamaClient) -> Result<String, String> {
            use crate::{AgentState, Message};

            self.agent.reset();

            let system_prompt = r#"You are mAgent, an AI agent on a smartwatch (nRF52840).

You MUST respond with ONLY valid JSON. No explanations, no markdown.

Available tools:
- read_sensor(sensor): temperature, heart_rate, spo2, accelerometer, steps, humidity, light
- write_gpio(pin, state): Control GPIO (high/low)
- flash_write(address, data): Write to flash
- ble_send(data): Send via BLE
- stt_start, stt_stop: Voice recognition
- tts_speak(text): Text to speech
- web_search(query): Search the web
- smarthome_on(device), smarthome_off(device): Control smart home

Rules:
1. Respond ONLY with JSON
2. Tool call: {"action": "tool_name", "args": {"param": "value"}}
3. Done: {"done": true, "result": "description"}"#;

            self.agent.messages.push(Message::user(task));
            self.agent.state = AgentState::Thinking;

            let mut conversation: Vec<(String, String)> = vec![];

            while self.agent.state != AgentState::Finished && self.agent.state != AgentState::Error {
                if self.agent.iteration >= self.agent.config.max_iterations { break; }
                if self.agent.tool_calls >= self.agent.config.max_tool_calls { break; }

                self.agent.iteration += 1;
                println!("\n[THINK] Iteration {}", self.agent.iteration);

                // Build context
                let health = self.agent.sim.read_health_data();
                let context = format!(
                    "Battery: {}%, Steps: {}, HR: {} bpm, SpO2: {}%",
                    health.battery.percentage, health.steps.steps,
                    health.heart_rate.rate, health.spo2.saturation
                );
                let user_message = format!("Task: {} (Context: {})", task, context);

                // Build messages for Ollama
                let mut messages: Vec<(String, String)> = conversation.clone();
                messages.push(("user".to_string(), user_message));

                let msg_refs: Vec<(&str, &str)> = messages.iter()
                    .map(|(r, c)| (r.as_str(), c.as_str()))
                    .collect();

                match ollama_client.chat(&msg_refs, system_prompt) {
                    Ok(response) => {
                        println!("[LLM Response] {}", response);
                        self.agent.messages.push(Message::assistant(&response));

                        // Parse tool call
                        if let Some(tc) = self.agent.parse_tool_call(&response) {
                            println!("[ACTION] Calling: {} with {:?}", tc.name, tc.args);
                            let args: Vec<(&str, &str)> = tc.args.iter()
                                .map(|(k, v)| (k.as_str(), v.as_str()))
                                .collect();

                            match self.agent.sim.execute_tool(&tc.name, &args) {
                                Ok(result) => {
                                    println!("[RESULT] {}", result);
                                    self.agent.messages.push(Message::tool(&result));
                                    conversation.push(("tool".to_string(), result.clone()));
                                    self.agent.tool_calls += 1;
                                    self.agent.state = AgentState::Observing;
                                }
                                Err(e) => {
                                    let msg = format!("Error: {}", e);
                                    println!("[ERROR] {}", msg);
                                    self.agent.messages.push(Message::tool(&msg));
                                    break;
                                }
                            }
                        } else if response.contains("done") || response.contains("DONE") {
                            self.agent.state = AgentState::Finished;
                        } else {
                            self.agent.state = AgentState::Observing;
                        }
                    }
                    Err(e) => {
                        println!("[OLLAMA ERROR] {} - falling back to simulation", e);
                        self.agent.think()?;
                    }
                }
            }

            self.agent.get_result()
        }
    }
}

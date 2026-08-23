//! mAgent Standalone Simulator
//!
//! A complete, runnable AI agent simulator that demonstrates:
//! - Real ReAct loop with LLM reasoning
//! - Sensor simulation
//! - GPIO control simulation
//! - Flash storage simulation
//! - BLE communication simulation
//!
//! Can connect to Ollama for real AI reasoning or use simulated responses.

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ============================================================================
// Agent State Machine
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Thinking,
    Executing,
    Observing,
    Finished,
    Error,
}

impl std::fmt::Display for AgentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentState::Idle => write!(f, "Idle"),
            AgentState::Thinking => write!(f, "Thinking"),
            AgentState::Executing => write!(f, "Executing"),
            AgentState::Observing => write!(f, "Observing"),
            AgentState::Finished => write!(f, "Finished"),
            AgentState::Error => write!(f, "Error"),
        }
    }
}

// ============================================================================
// Tool System
// ============================================================================

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub name: String,
    pub arguments: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_name: String,
    pub content: String,
    pub success: bool,
}

impl ToolResult {
    pub fn success(tool_name: &str, content: &str) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            content: content.to_string(),
            success: true,
        }
    }

    pub fn error(tool_name: &str, error: &str) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            content: error.to_string(),
            success: false,
        }
    }
}

// ============================================================================
// Simulator Components
// ============================================================================

#[derive(Debug, Clone)]
pub enum SensorType {
    Temperature,
    Humidity,
    Pressure,
    Accelerometer,
    Light,
}

pub struct SimulatedSensors {
    iteration: usize,
    base_temp: f64,
    base_humidity: f64,
    base_pressure: f64,
}

impl SimulatedSensors {
    pub fn new() -> Self {
        Self {
            iteration: 0,
            base_temp: 23.5,
            base_humidity: 55.0,
            base_pressure: 1013.25,
        }
    }

    pub fn read(&mut self, sensor: &SensorType) -> String {
        self.iteration += 1;
        match sensor {
            SensorType::Temperature => {
                let variation = ((self.iteration as f64 * 0.1).sin() * 2.0)
                    + ((self.iteration as f64 * 0.3).cos() * 0.5);
                let temp = self.base_temp + variation;
                format!("{:.1}°C", temp)
            }
            SensorType::Humidity => {
                let variation = ((self.iteration as f64 * 0.05).sin() * 5.0) + 2.0;
                format!("{:.1}%", self.base_humidity + variation)
            }
            SensorType::Pressure => {
                let variation = (self.iteration as f64 * 0.02).sin() * 2.0;
                format!("{:.1} hPa", self.base_pressure + variation)
            }
            SensorType::Accelerometer => {
                let noise = || (self.iteration as f64 * 17.3).sin() * 0.01;
                format!(
                    "X={:.3}g Y={:.3}g Z={:.3}g",
                    0.0 + noise(),
                    0.0 + noise(),
                    9.8 + noise()
                )
            }
            SensorType::Light => {
                let cycle = ((self.iteration as f64 * 0.01).sin() * 500.0 + 500.0).max(10.0);
                format!("{:.1} lux", cycle)
            }
        }
    }
}

impl Default for SimulatedSensors {
    fn default() -> Self {
        Self::new()
    }
}

pub struct GpioController {
    pins: Vec<GpioPinState>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GpioPinState {
    Low,
    High,
}

impl GpioController {
    pub fn new(num_pins: usize) -> Self {
        Self {
            pins: vec![GpioPinState::Low; num_pins],
        }
    }

    pub fn set(&mut self, pin: usize, state: GpioPinState) -> Result<()> {
        if pin >= self.pins.len() {
            return Err(anyhow!("Invalid pin number: {}", pin));
        }
        self.pins[pin] = state;
        Ok(())
    }

    pub fn get(&self, pin: usize) -> Result<GpioPinState> {
        if pin >= self.pins.len() {
            return Err(anyhow!("Invalid pin number: {}", pin));
        }
        Ok(self.pins[pin])
    }

    pub fn toggle(&mut self, pin: usize) -> Result<()> {
        let current = self.get(pin)?;
        let new_state = match current {
            GpioPinState::Low => GpioPinState::High,
            GpioPinState::High => GpioPinState::Low,
        };
        self.set(pin, new_state)
    }

    pub fn status(&self) -> Vec<(usize, GpioPinState)> {
        self.pins
            .iter()
            .enumerate()
            .map(|(i, s)| (i, *s))
            .collect()
    }
}

pub struct FlashStorage {
    data: Vec<u8>,
    writes: usize,
}

impl FlashStorage {
    pub fn new(size: usize) -> Self {
        Self {
            data: vec![0xFF; size],
            writes: 0,
        }
    }

    pub fn read(&self, address: usize, len: usize) -> Result<Vec<u8>> {
        if address + len > self.data.len() {
            return Err(anyhow!("Address out of bounds"));
        }
        Ok(self.data[address..address + len].to_vec())
    }

    pub fn write(&mut self, address: usize, data: &[u8]) -> Result<()> {
        if address + data.len() > self.data.len() {
            return Err(anyhow!("Write would exceed flash size"));
        }
        for (i, &byte) in data.iter().enumerate() {
            // Flash can only clear bits, not set them
            self.data[address + i] &= byte;
        }
        self.writes += 1;
        Ok(())
    }

    pub fn write_count(&self) -> usize {
        self.writes
    }

    pub fn erase_sector(&mut self, sector: usize, sector_size: usize) -> Result<()> {
        let start = sector * sector_size;
        if start >= self.data.len() {
            return Err(anyhow!("Invalid sector"));
        }
        let end = (start + sector_size).min(self.data.len());
        for i in start..end {
            self.data[i] = 0xFF;
        }
        Ok(())
    }
}

pub struct BleInterface {
    connected: bool,
    messages: Vec<String>,
}

impl BleInterface {
    pub fn new() -> Self {
        Self {
            connected: false,
            messages: Vec::new(),
        }
    }

    pub fn connect(&mut self) {
        self.connected = true;
    }

    pub fn disconnect(&mut self) {
        self.connected = false;
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn send(&mut self, data: &str) -> Result<()> {
        if !self.connected {
            return Err(anyhow!("BLE not connected"));
        }
        self.messages.push(data.to_string());
        Ok(())
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn last_message(&self) -> Option<&str> {
        self.messages.last().map(|s| s.as_str())
    }
}

impl Default for BleInterface {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tool Executor
// ============================================================================

pub struct ToolExecutor {
    sensors: Arc<Mutex<SimulatedSensors>>,
    gpio: Arc<Mutex<GpioController>>,
    flash: Arc<Mutex<FlashStorage>>,
    ble: Arc<Mutex<BleInterface>>,
}

impl ToolExecutor {
    pub fn new(
        sensors: Arc<Mutex<SimulatedSensors>>,
        gpio: Arc<Mutex<GpioController>>,
        flash: Arc<Mutex<FlashStorage>>,
        ble: Arc<Mutex<BleInterface>>,
    ) -> Self {
        Self {
            sensors,
            gpio,
            flash,
            ble,
        }
    }

    pub fn execute(&self, tool_name: &str, args: &HashMap<String, serde_json::Value>) -> ToolResult {
        match tool_name {
            "read_sensor" => {
                let sensor_str = args
                    .get("sensor")
                    .and_then(|v| v.as_str())
                    .unwrap_or("temperature");
                let sensor = match sensor_str {
                    "temperature" => SensorType::Temperature,
                    "humidity" => SensorType::Humidity,
                    "pressure" => SensorType::Pressure,
                    "accelerometer" => SensorType::Accelerometer,
                    "light" => SensorType::Light,
                    _ => SensorType::Temperature,
                };
                let mut sensors = self.sensors.lock().unwrap();
                let value = sensors.read(&sensor);
                ToolResult::success("read_sensor", &value)
            }
            "write_gpio" => {
                let pin = args.get("pin").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let state_str = args.get("state").and_then(|v| v.as_str()).unwrap_or("low");
                let state = if state_str == "high" {
                    GpioPinState::High
                } else {
                    GpioPinState::Low
                };
                let mut gpio = self.gpio.lock().unwrap();
                match gpio.set(pin, state) {
                    Ok(_) => {
                        let state_str = if matches!(state, GpioPinState::High) {
                            "high"
                        } else {
                            "low"
                        };
                        ToolResult::success("write_gpio", &format!("Pin {} set to {}", pin, state_str))
                    }
                    Err(e) => ToolResult::error("write_gpio", &e.to_string()),
                }
            }
            "flash_read" => {
                let address = args.get("address").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let flash = self.flash.lock().unwrap();
                match flash.read(address, 64) {
                    Ok(data) => {
                        let hex: String = data.iter().take(16).map(|b| format!("{:02X}", b)).collect();
                        ToolResult::success("flash_read", &format!("Read at 0x{:04X}: {}", address, hex))
                    }
                    Err(e) => ToolResult::error("flash_read", &e.to_string()),
                }
            }
            "flash_write" => {
                let address = args.get("address").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let data = args.get("data").and_then(|v| v.as_str()).unwrap_or("");
                let mut flash = self.flash.lock().unwrap();
                match flash.write(address, data.as_bytes()) {
                    Ok(_) => ToolResult::success(
                        "flash_write",
                        &format!("Wrote {} bytes to flash at 0x{:04X}", data.len(), address),
                    ),
                    Err(e) => ToolResult::error("flash_write", &e.to_string()),
                }
            }
            "ble_send" => {
                let data = args.get("data").and_then(|v| v.as_str()).unwrap_or("");
                let mut ble = self.ble.lock().unwrap();
                match ble.send(data) {
                    Ok(_) => ToolResult::success("ble_send", &format!("Sent {} bytes via BLE", data.len())),
                    Err(e) => ToolResult::error("ble_send", &e.to_string()),
                }
            }
            _ => ToolResult::error(tool_name, &format!("Unknown tool: {}", tool_name)),
        }
    }
}

// ============================================================================
// LLM Integration
// ============================================================================

pub struct OllamaClient {
    base_url: String,
    model: String,
    client: reqwest::blocking::Client,
}

impl OllamaClient {
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            model: model.to_string(),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap(),
        }
    }

    pub fn generate(&self, prompt: &str) -> Result<String> {
        let request_body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "stream": false,
            "options": {
                "temperature": 0.7,
                "num_predict": 512
            }
        });

        let response = self
            .client
            .post(format!("{}/api/generate", self.base_url))
            .json(&request_body)
            .send()?;

        if !response.status().is_success() {
            return Err(anyhow!("Ollama API error: {}", response.status()));
        }

        let json: serde_json::Value = response.json()?;
        let content = json["response"]
            .as_str()
            .ok_or_else(|| anyhow!("No response field"))?;

        Ok(content.to_string())
    }

    /// Chat completion with messages
    pub fn chat(&self, messages: &[String], system_prompt: &str) -> Result<String> {
        let mut chat_messages: Vec<serde_json::Value> = vec![serde_json::json!({
            "role": "system",
            "content": system_prompt
        })];

        for msg in messages {
            if msg.starts_with("[User]") {
                chat_messages.push(serde_json::json!({
                    "role": "user",
                    "content": msg.trim_start_matches("[User]").trim()
                }));
            } else if msg.starts_with("[Assistant]") {
                chat_messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": msg.trim_start_matches("[Assistant]").trim()
                }));
            } else if msg.starts_with("[Tool]") {
                chat_messages.push(serde_json::json!({
                    "role": "tool",
                    "content": msg.trim_start_matches("[Tool]").trim()
                }));
            } else if msg.starts_with("[System]") {
                // Extract the task from system message
                let task = msg.replace("[System] Task: ", "");
                chat_messages.push(serde_json::json!({
                    "role": "user",
                    "content": task
                }));
            } else {
                chat_messages.push(serde_json::json!({
                    "role": "user",
                    "content": msg
                }));
            }
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
            .send()?;

        if !response.status().is_success() {
            return Err(anyhow!("Ollama chat API error: {}", response.status()));
        }

        let json: serde_json::Value = response.json()?;
        let content = json["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow!("No content in response"))?;

        Ok(content.to_string())
    }

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

    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }
}

// ============================================================================
// Agent Runner
// ============================================================================

pub struct Agent {
    executor: Arc<ToolExecutor>,
    ollama: Option<OllamaClient>,
    state: AgentState,
    messages: Vec<String>,
    iteration: usize,
    max_iterations: usize,
    verbose: bool,
}

impl Agent {
    pub fn new(executor: Arc<ToolExecutor>, use_ollama: bool) -> Self {
        let ollama = if use_ollama {
            Some(OllamaClient::new("http://localhost:11434", "llama3:latest"))
        } else {
            None
        };

        Self {
            executor,
            ollama,
            state: AgentState::Idle,
            messages: Vec::new(),
            iteration: 0,
            max_iterations: 10,
            verbose: true,
        }
    }

    pub fn run(&mut self, task: &str) -> Result<String> {
        self.reset();

        if self.verbose {
            println!("\n{}", "=".repeat(60));
            println!("mAgent Starting Task: {}", task);
            println!("{}", "=".repeat(60));
        }

        self.add_message(&format!("[System] Task: {}", task));
        self.state = AgentState::Thinking;

        // Main ReAct loop
        while self.state != AgentState::Finished && self.state != AgentState::Error {
            if self.iteration >= self.max_iterations {
                if self.verbose {
                    println!("[Agent] Max iterations reached");
                }
                break;
            }

            self.iteration += 1;

            match self.state {
                AgentState::Thinking => self.think()?,
                AgentState::Executing => self.execute()?,
                AgentState::Observing => self.observe(),
                _ => break,
            }
        }

        self.get_result()
    }

    fn think(&mut self) -> Result<()> {
        if self.verbose {
            println!("\n[Thinking] Iteration {}", self.iteration);
        }

        let system_prompt = r#"You are mAgent, an embedded AI agent running on a microcontroller (nRF52840).

You MUST respond with ONLY valid JSON. No explanations, no markdown, no text outside JSON.

Available tools:
- read_sensor(sensor): Read sensor - sensor can be: temperature, accelerometer, humidity, pressure, light
- write_gpio(pin, state): Control GPIO - pin (0-31), state (high or low)
- flash_read(address): Read from flash memory - address (integer)
- flash_write(address, data): Write to flash memory - address (integer), data (string)
- ble_send(data): Send via Bluetooth - data (string)

Rules:
1. ALWAYS respond with ONLY JSON
2. To call a tool, use: {"tool": "tool_name", "args": {"param": "value"}}
3. When task is done, use: {"result": "description of what was done"}
4. Be concise and efficient
5. Execute one tool at a time"#;

        let response = if let Some(ref ollama) = self.ollama {
            match ollama.chat(&self.messages, system_prompt) {
                Ok(r) => r,
                Err(e) => {
                    if self.verbose {
                        println!("[Warning] Ollama error: {}, using simulated response", e);
                    }
                    self.generate_simulated_response(&self.messages)
                }
            }
        } else {
            self.generate_simulated_response(&self.messages)
        };

        if self.verbose {
            println!("[LLM Response] {}", response);
        }

        self.add_message(&format!("[Assistant] {}", response));

        // Parse for tool call or result
        // First check for tool call
        if let Some(tool_call) = self.parse_tool_call(&response) {
            self.add_message(&format!(
                "[Action] Calling tool: {} with {:?}",
                tool_call.name, tool_call.arguments
            ));
            self.execute_tool_call(&tool_call)?;
            self.state = AgentState::Executing;
        }
        // Then check for result (if no tool call or after tool call)
        else if let Some(result) = self.parse_result(&response) {
            self.add_message(&format!("[Result] {}", result));
            self.state = AgentState::Finished;
        } else {
            // Continue thinking
            self.state = AgentState::Observing;
        }

        Ok(())
    }

    fn execute(&mut self) -> Result<()> {
        // Tool already executed in think()
        self.state = AgentState::Observing;
        Ok(())
    }

    fn execute_tool_call(&mut self, tool_call: &ToolCall) -> Result<()> {
        if self.verbose {
            println!("[Executing] Tool: {}", tool_call.name);
        }

        let result = self.executor.execute(&tool_call.name, &tool_call.arguments);

        if self.verbose {
            if result.success {
                println!("[Tool Result] {}: {}", result.tool_name, result.content);
            } else {
                println!("[Tool Error] {}: {}", result.tool_name, result.content);
            }
        }

        self.add_message(&format!("[Tool] {}: {}", result.tool_name, result.content));
        Ok(())
    }

    fn observe(&mut self) {
        if self.verbose {
            println!("[Observing] Processing result...");
        }
        self.state = AgentState::Thinking;
    }

    fn generate_simulated_response(&self, messages: &[String]) -> String {
        // Count existing tool results
        let tool_count = messages.iter().filter(|m| m.starts_with("[Tool]")).count();

        // Get the original user task (first user message)
        let user_task = messages.iter()
            .find(|m| m.starts_with("[System]"))
            .map(|s| s.replace("[System] Task: ", "").to_lowercase())
            .unwrap_or_default();

        // Environmental monitoring (multi-sensor)
        if user_task.contains("monitor") || user_task.contains("environment") {
            if tool_count == 0 {
                return r#"{"tool": "read_sensor", "args": {"sensor": "temperature"}}"#.to_string();
            }
            if tool_count == 1 {
                return r#"{"tool": "read_sensor", "args": {"sensor": "humidity"}}"#.to_string();
            }
            if tool_count == 2 {
                return r#"{"tool": "read_sensor", "args": {"sensor": "pressure"}}"#.to_string();
            }
            if tool_count == 3 {
                return r#"{"tool": "ble_send", "args": {"data": "Environmental data logged"}}"#.to_string();
            }
            return r#"{"result": "Environmental monitoring complete"}"#.to_string();
        }

        // Single sensor reads
        if user_task.contains("temperature") && !user_task.contains("humidity") && !user_task.contains("pressure") {
            if tool_count == 0 {
                return r#"{"tool": "read_sensor", "args": {"sensor": "temperature"}}"#.to_string();
            }
            return r#"{"result": "Temperature sensor reading completed"}"#.to_string();
        }

        if user_task.contains("humidity") && !user_task.contains("temperature") && !user_task.contains("pressure") {
            if tool_count == 0 {
                return r#"{"tool": "read_sensor", "args": {"sensor": "humidity"}}"#.to_string();
            }
            return r#"{"result": "Humidity sensor reading completed"}"#.to_string();
        }

        if user_task.contains("pressure") && !user_task.contains("humidity") && !user_task.contains("temperature") {
            if tool_count == 0 {
                return r#"{"tool": "read_sensor", "args": {"sensor": "pressure"}}"#.to_string();
            }
            return r#"{"result": "Pressure sensor reading completed"}"#.to_string();
        }

        // LED control
        if user_task.contains("led") && user_task.contains("on") {
            if tool_count == 0 {
                return r#"{"tool": "write_gpio", "args": {"pin": 13, "state": "high"}}"#.to_string();
            }
            return r#"{"result": "LED turned on successfully"}"#.to_string();
        }

        if user_task.contains("led") && user_task.contains("off") {
            if tool_count == 0 {
                return r#"{"tool": "write_gpio", "args": {"pin": 13, "state": "low"}}"#.to_string();
            }
            return r#"{"result": "LED turned off successfully"}"#.to_string();
        }

        // BLE notification
        if user_task.contains("ble") && (user_task.contains("notification") || user_task.contains("alert") || user_task.contains("send")) {
            if tool_count == 0 {
                return r#"{"tool": "ble_send", "args": {"data": "Alert from mAgent"}}"#.to_string();
            }
            return r#"{"result": "BLE notification sent successfully"}"#.to_string();
        }

        // Flash storage
        if user_task.contains("flash") && (user_task.contains("log") || user_task.contains("save")) {
            if tool_count == 0 {
                return r#"{"tool": "flash_write", "args": {"address": 1024, "data": "Log entry"}}"#.to_string();
            }
            return r#"{"result": "Data logged to flash memory"}"#.to_string();
        }

        // Default task - temperature
        if tool_count == 0 {
            return r#"{"tool": "read_sensor", "args": {"sensor": "temperature"}}"#.to_string();
        }
        r#"{"result": "Task completed"}"#.to_string()
    }

    fn parse_tool_call(&self, response: &str) -> Option<ToolCall> {
        // Try to parse as JSON
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(response) {
            // Format 1: {"tool": "name", "args": {...}}
            if let (Some(tool), Some(args)) = (json.get("tool"), json.get("args")) {
                let name = tool.as_str()?.to_string();
                let args_map: HashMap<String, serde_json::Value> = args
                    .as_object()?
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                return Some(ToolCall { name, arguments: args_map });
            }

            // Format 2: {"tool_name": {"args": {...}}} - common LLM variation
            let tool_names = ["read_sensor", "write_gpio", "flash_read", "flash_write", "ble_send"];
            for tool_name in tool_names {
                if let Some(tool_obj) = json.get(tool_name) {
                    if let Some(args) = tool_obj.get("args").or(Some(tool_obj)) {
                        let args_map: HashMap<String, serde_json::Value> = if let Some(obj) = args.as_object() {
                            obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                        } else {
                            HashMap::new()
                        };
                        return Some(ToolCall {
                            name: tool_name.to_string(),
                            arguments: args_map,
                        });
                    }
                }
            }
        }
        None
    }

    fn parse_result(&self, response: &str) -> Option<String> {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(response) {
            if let Some(result) = json.get("result") {
                // Handle string result
                if let Some(s) = result.as_str() {
                    return Some(s.to_string());
                }
                // Handle numeric result - convert to string
                if let Some(n) = result.as_f64() {
                    return Some(format!("{:.1}", n));
                }
                if let Some(n) = result.as_i64() {
                    return Some(n.to_string());
                }
                // Handle object/array result - format as string
                return Some(result.to_string());
            }
        }
        None
    }

    fn add_message(&mut self, msg: &str) {
        self.messages.push(msg.to_string());
    }

    fn get_result(&self) -> Result<String> {
        for msg in self.messages.iter().rev() {
            if msg.starts_with("[Result]") {
                return Ok(msg.replace("[Result] ", ""));
            }
        }
        // Return last assistant message
        for msg in self.messages.iter().rev() {
            if msg.starts_with("[Assistant]") && !msg.contains("tool") {
                return Ok(msg.replace("[Assistant] ", ""));
            }
        }
        Ok("Task completed".to_string())
    }

    fn reset(&mut self) {
        self.state = AgentState::Idle;
        self.messages.clear();
        self.iteration = 0;
    }

    pub fn get_status(&self) -> String {
        let _sensors = self.executor.sensors.lock().unwrap();
        let gpio = self.executor.gpio.lock().unwrap();
        let flash = self.executor.flash.lock().unwrap();
        let ble = self.executor.ble.lock().unwrap();

        format!(
            "Status:\n  State: {}\n  Iterations: {}\n  Flash writes: {}\n  BLE messages: {}\n  BLE connected: {}\n  GPIO pins: {} high",
            self.state,
            self.iteration,
            flash.write_count(),
            ble.message_count(),
            ble.is_connected(),
            gpio.status().iter().filter(|(_, s)| matches!(s, GpioPinState::High)).count()
        )
    }
}

// ============================================================================
// Main Entry Point
// ============================================================================

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    println!("\n{}", "=".repeat(60));
    println!("       mAgent - Embedded AI Agent Simulator");
    println!("{}", "=".repeat(60));

    // Initialize components
    let sensors = Arc::new(Mutex::new(SimulatedSensors::new()));
    let gpio = Arc::new(Mutex::new(GpioController::new(32)));
    let flash = Arc::new(Mutex::new(FlashStorage::new(65536)));
    let ble = Arc::new(Mutex::new(BleInterface::new()));

    let executor = Arc::new(ToolExecutor::new(
        sensors.clone(),
        gpio.clone(),
        flash.clone(),
        ble.clone(),
    ));

    // Check Ollama connection and get available models
    let ollama_client = OllamaClient::new("http://localhost:11434", "llama3:latest");
    let ollama_available = ollama_client.check_connection();

    if ollama_available {
        println!("\n✓ Ollama connected");
        let models = ollama_client.get_models();
        println!("  Available models: {:?}", models);
        println!("  Using model: {}", ollama_client.model);
    } else {
        println!("\n✗ Ollama not available - using simulated AI reasoning");
    }

    // Create agent
    let mut agent = Agent::new(executor, ollama_available);
    agent.max_iterations = 10;
    agent.verbose = true;

    // Connect BLE
    {
        let mut ble = ble.lock().unwrap();
        ble.connect();
    }

    // Run demo scenarios
    println!("\n{}", "-".repeat(60));
    println!("Running Demo Scenarios...");
    println!("{}", "-".repeat(60));

    // Scenario 1: Read temperature
    println!("\n📊 Scenario 1: Read Temperature Sensor");
    match agent.run("Read the temperature sensor") {
        Ok(result) => println!("Result: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Scenario 2: Read multiple sensors
    println!("\n📊 Scenario 2: Environmental Monitoring");
    match agent.run("Monitor the environment: read temperature, humidity, and pressure") {
        Ok(result) => println!("Result: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Scenario 3: Control LED
    println!("\n💡 Scenario 3: Control LED");
    match agent.run("Turn on the LED on pin 13") {
        Ok(result) => println!("Result: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Scenario 4: Send BLE notification
    println!("\n📡 Scenario 4: Send BLE Notification");
    match agent.run("Send a status notification via BLE") {
        Ok(result) => println!("Result: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Scenario 5: Flash storage
    println!("\n💾 Scenario 5: Flash Storage");
    match agent.run("Log the current sensor readings to flash memory") {
        Ok(result) => println!("Result: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Scenario 6: Complex task
    println!("\n🔄 Scenario 6: Complex Multi-Step Task");
    match agent.run("Check the temperature, and if it's above 30°C turn on the cooling fan (GPIO 14)") {
        Ok(result) => println!("Result: {}", result),
        Err(e) => println!("Error: {}", e),
    }

    // Print final status
    println!("\n{}", "-".repeat(60));
    println!("{}", agent.get_status());
    println!("{}", "=".repeat(60));

    println!("\nDemo completed successfully!");

    Ok(())
}

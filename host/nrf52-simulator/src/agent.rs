//! AI Agent for Smartwatch using nRF52840
//!
//! A complete ReAct-based AI agent that runs on a smartwatch,
//! using simulated nRF52840 hardware for testing.

use nrf52_simulator::{
    SmartwatchSimulator, ThreadSafeSimulator, AVAILABLE_TOOLS,
    HealthData, SystemInfo, PinState, BleState, PowerMode,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Agent Types
// ============================================================================

/// Agent state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Idle,
    Thinking,
    Acting,
    Observing,
    Finished,
    Error,
}

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
        Self {
            success: true,
            content: content.to_string(),
            error: None,
        }
    }

    pub fn err(error: &str) -> Self {
        Self {
            success: false,
            content: String::new(),
            error: Some(error.to_string()),
        }
    }
}

/// Message in conversation
#[derive(Debug, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn user(content: &str) -> Self {
        Self {
            role: "user".to_string(),
            content: content.to_string(),
        }
    }

    pub fn assistant(content: &str) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.to_string(),
        }
    }

    pub fn system(content: &str) -> Self {
        Self {
            role: "system".to_string(),
            content: content.to_string(),
        }
    }

    pub fn tool(content: &str) -> Self {
        Self {
            role: "tool".to_string(),
            content: content.to_string(),
        }
    }
}

/// Agent configuration
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub name: String,
    pub max_iterations: usize,
    pub max_tool_calls: usize,
    pub verbose: bool,
    pub use_llm: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "mAgent".to_string(),
            max_iterations: 20,
            max_tool_calls: 10,
            verbose: true,
            use_llm: false, // Use simulated reasoning by default
        }
    }
}

/// Complete AI Agent
pub struct SmartwatchAgent {
    config: AgentConfig,
    state: AgentState,
    simulator: SmartwatchSimulator,
    messages: Vec<Message>,
    tool_call_count: usize,
    iteration: usize,
}

impl SmartwatchAgent {
    /// Create a new agent
    pub fn new(config: AgentConfig) -> Self {
        Self {
            config,
            state: AgentState::Idle,
            simulator: SmartwatchSimulator::new(),
            messages: Vec::new(),
            tool_call_count: 0,
            iteration: 0,
        }
    }

    /// Create with default config
    pub fn with_defaults() -> Self {
        Self::new(AgentConfig::default())
    }

    /// Run a task
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

        // Main loop
        while self.state != AgentState::Finished && self.state != AgentState::Error {
            // Check limits
            if self.iteration >= self.config.max_iterations {
                if self.config.verbose {
                    println!("[LIMIT] Max iterations reached");
                }
                break;
            }
            if self.tool_call_count >= self.config.max_tool_calls {
                if self.config.verbose {
                    println!("[LIMIT] Max tool calls reached");
                }
                break;
            }

            self.iteration += 1;

            match self.state {
                AgentState::Thinking => self.think()?,
                AgentState::Acting => self.act()?,
                AgentState::Observing => self.observe(),
                _ => break,
            }
        }

        self.get_result()
    }

    /// Reset agent state
    fn reset(&mut self) {
        self.state = AgentState::Idle;
        self.messages.clear();
        self.tool_call_count = 0;
        self.iteration = 0;
        self.simulator = SmartwatchSimulator::new();
    }

    /// Think phase - decide what to do
    fn think(&mut self) -> Result<(), String> {
        if self.config.verbose {
            println!("\n[THINK] Iteration {}", self.iteration);
        }

        // Get context from simulation
        let health = self.simulator.read_health_data();
        let context = format!(
            "Current state: Battery {}%, Steps: {}, Heart Rate: {} bpm, SpO2: {}%",
            health.battery.percentage,
            health.steps.steps,
            health.heart_rate.rate,
            health.spo2.saturation
        );

        // Simulate LLM reasoning with task-specific responses
        let response = self.generate_response(&context);

        if self.config.verbose {
            println!("[REASONING] {}", response);
        }

        self.messages.push(Message::assistant(&response));

        // Parse for tool call
        if let Some(tool_call) = self.parse_tool_call(&response) {
            self.messages.push(Message::system(&format!(
                "Executing: {} with {:?}",
                tool_call.name, tool_call.args
            )));
            self.execute_tool(&tool_call)?;
            self.state = AgentState::Acting;
        } else if response.contains("DONE") || response.contains("RESULT") {
            self.state = AgentState::Finished;
        } else {
            self.state = AgentState::Observing;
        }

        Ok(())
    }

    /// Act phase - execute the tool
    fn act(&mut self) -> Result<(), String> {
        if self.config.verbose {
            println!("[ACT] Tool executed");
        }
        self.tool_call_count += 1;
        self.state = AgentState::Observing;
        Ok(())
    }

    /// Observe phase - process results
    fn observe(&mut self) {
        if self.config.verbose {
            println!("[OBSERVE] Processing result...");
        }
        self.state = AgentState::Thinking;
    }

    /// Execute a tool call
    fn execute_tool(&mut self, tool_call: &ToolCall) -> Result<(), String> {
        let args: Vec<(&str, &str)> = tool_call
            .args
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        match self.simulator.execute_tool(&tool_call.name, &args) {
            Ok(result) => {
                if self.config.verbose {
                    println!("[RESULT] {}", result);
                }
                self.messages.push(Message::tool(&result));
                Ok(())
            }
            Err(e) => {
                let err_msg = format!("Error: {}", e);
                if self.config.verbose {
                    println!("[ERROR] {}", err_msg);
                }
                self.messages.push(Message::tool(&err_msg));
                Err(e)
            }
        }
    }

    /// Generate a simulated LLM response
    fn generate_response(&self, context: &str) -> String {
        let task = self.messages
            .iter()
            .find(|m| m.role == "user")
            .map(|m| m.content.to_lowercase())
            .unwrap_or_default();

        // Count tool results
        let tool_count = self.messages.iter().filter(|m| m.role == "tool").count();

        // Health monitoring scenarios
        if task.contains("health") || task.contains("vital") || task.contains("monitor") {
            if tool_count == 0 {
                return r#"{"action": "read_sensor", "sensor": "heart_rate"}"#.to_string();
            }
            if tool_count == 1 {
                return r#"{"action": "read_sensor", "sensor": "spo2"}"#.to_string();
            }
            if tool_count == 2 {
                return r#"{"action": "get_battery"}"#.to_string();
            }
            if tool_count == 3 {
                return r#"{"action": "ble_send", "data": "Health data logged"}"#.to_string();
            }
            return format!(
                r#"{{"done": true, "result": "Health check complete. {}"}}"#,
                context
            );
        }

        // Temperature check
        if task.contains("temperature") || task.contains("temp") {
            if tool_count == 0 {
                return r#"{"action": "read_sensor", "sensor": "temperature"}"#.to_string();
            }
            if task.contains("high") || task.contains("above") {
                if tool_count == 1 {
                    return r#"{"action": "write_gpio", "pin": "14", "state": "high"}"#.to_string();
                }
                return r#"{"done": true, "result": "Temperature was above threshold, fan turned on"}"#.to_string();
            }
            return format!(r#"{{"done": true, "result": "Temperature reading complete. {}"}}"#, context);
        }

        // LED control
        if task.contains("led") || task.contains("light") {
            if task.contains("on") || task.contains("turn on") || task.contains("enable") {
                return r#"{"action": "write_gpio", "pin": "13", "state": "high"}"#.to_string();
            }
            if task.contains("off") || task.contains("turn off") || task.contains("disable") {
                return r#"{"action": "write_gpio", "pin": "13", "state": "low"}"#.to_string();
            }
        }

        // BLE notification
        if task.contains("ble") || task.contains("notify") || task.contains("alert") || task.contains("send") {
            if tool_count == 0 {
                return r#"{"action": "ble_send", "data": "Notification from mAgent"}"#.to_string();
            }
            return r#"{"done": true, "result": "BLE notification sent successfully"}"#.to_string();
        }

        // Flash storage
        if task.contains("flash") || task.contains("log") || task.contains("save") || task.contains("store") {
            if tool_count == 0 {
                return r#"{"action": "read_sensor", "sensor": "steps"}"#.to_string();
            }
            if tool_count == 1 {
                return r#"{"action": "flash_write", "address": "0", "data": "Activity log"}"#.to_string();
            }
            return r#"{"done": true, "result": "Data logged to flash successfully"}"#.to_string();
        }

        // Steps or activity
        if task.contains("step") || task.contains("walk") || task.contains("exercise") || task.contains("activity") {
            if tool_count == 0 {
                return r#"{"action": "read_sensor", "sensor": "steps"}"#.to_string();
            }
            if tool_count == 1 {
                return r#"{"action": "read_sensor", "sensor": "accelerometer"}"#.to_string();
            }
            return format!(r#"{{"done": true, "result": "Activity tracking complete. {}"}}"#, context);
        }

        // System status
        if task.contains("status") || task.contains("system") || task.contains("check") || task.contains("report") {
            if tool_count == 0 {
                return r#"{"action": "get_status"}"#.to_string();
            }
            if tool_count == 1 {
                return r#"{"action": "get_battery"}"#.to_string();
            }
            return format!(r#"{{"done": true, "result": "System status: {}"}}"#, context);
        }

        // Default: read battery
        if tool_count == 0 {
            return r#"{"action": "get_battery"}"#.to_string();
        }

        format!(r#"{{"done": true, "result": "Task completed. {}"}}"#, context)
    }

    /// Parse tool call from response
    fn parse_tool_call(&self, response: &str) -> Option<ToolCall> {
        // Try JSON format
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(response) {
            if let Some(action) = json.get("action").or(json.get("tool")).and_then(|v| v.as_str()) {
                let mut args = Vec::new();

                if let Some(obj) = json.get("args").or(json.get("params")).or(json.get("sensor"))
                    .or(json.get("pin")).or(json.get("data")).or(json.get("address"))
                {
                    // Handle different formats
                    if let Some(s) = obj.as_str() {
                        if let Some(val) = json.get("sensor").or(json.get("state")).or(json.get("pin"))
                            .or(json.get("data")).or(json.get("address"))
                            .and_then(|v| v.as_str())
                        {
                            args.push((s.to_string(), val.to_string()));
                        }
                    } else if let Some(obj) = obj.as_object() {
                        for (k, v) in obj {
                            if let Some(s) = v.as_str() {
                                args.push((k.clone(), s.to_string()));
                            } else if let Some(n) = v.as_i64() {
                                args.push((k.clone(), n.to_string()));
                            }
                        }
                    }
                }

                // Handle flat JSON format
                if let Some(sensor) = json.get("sensor").and_then(|v| v.as_str()) {
                    args.push(("sensor".to_string(), sensor.to_string()));
                }
                if let Some(pin) = json.get("pin").and_then(|v| v.as_str().or(v.as_i64().map(|n| n.to_string()).as_deref())) {
                    args.push(("pin".to_string(), pin.to_string()));
                }
                if let Some(state) = json.get("state").and_then(|v| v.as_str()) {
                    args.push(("state".to_string(), state.to_string()));
                }
                if let Some(data) = json.get("data").and_then(|v| v.as_str()) {
                    args.push(("data".to_string(), data.to_string()));
                }
                if let Some(address) = json.get("address").and_then(|v| v.as_str().or(v.as_i64().map(|n| n.to_string()).as_deref())) {
                    args.push(("address".to_string(), address.to_string()));
                }

                return Some(ToolCall {
                    name: action.to_string(),
                    args,
                });
            }
        }

        None
    }

    /// Get final result
    fn get_result(&self) -> Result<String, String> {
        // Find the result in messages
        for msg in self.messages.iter().rev() {
            if msg.role == "assistant" {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                    if let Some(result) = json.get("result").and_then(|v| v.as_str()) {
                        return Ok(result.to_string());
                    }
                }
                if msg.content.contains("DONE") || msg.content.contains("RESULT") {
                    return Ok(msg.content.clone());
                }
            }
        }

        // Return last assistant message
        for msg in self.messages.iter().rev() {
            if msg.role == "assistant" {
                return Ok(msg.content.clone());
            }
        }

        Ok("Task completed".to_string())
    }

    /// Get health data
    pub fn get_health_data(&self) -> HealthData {
        self.simulator.read_health_data()
    }

    /// Get system info
    pub fn get_system_info(&self) -> SystemInfo {
        self.simulator.get_system_info()
    }

    /// Get current state
    pub fn state(&self) -> AgentState {
        self.state
    }

    /// Get iteration count
    pub fn iteration(&self) -> usize {
        self.iteration
    }

    /// Get tool call count
    pub fn tool_call_count(&self) -> usize {
        self.tool_call_count
    }

    /// Get messages
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Get simulator reference
    pub fn simulator(&self) -> &SmartwatchSimulator {
        &self.simulator
    }

    /// Get mutable simulator reference
    pub fn simulator_mut(&mut self) -> &mut SmartwatchSimulator {
        &mut self.simulator
    }
}

// ============================================================================
// Thread-safe Agent Wrapper
// ============================================================================

/// Thread-safe agent wrapper
pub struct ThreadSafeAgent {
    inner: Arc<Mutex<SmartwatchAgent>>,
}

impl ThreadSafeAgent {
    pub fn new(config: AgentConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SmartwatchAgent::new(config))),
        }
    }

    pub fn run(&self, task: &str) -> Result<String, String> {
        if let Ok(mut agent) = self.inner.lock() {
            agent.run(task)
        } else {
            Err("Failed to lock agent".to_string())
        }
    }

    pub fn get_health_data(&self) -> Option<HealthData> {
        if let Ok(agent) = self.inner.lock() {
            Some(agent.get_health_data())
        } else {
            None
        }
    }
}

impl Default for ThreadSafeAgent {
    fn default() -> Self {
        Self::new(AgentConfig::default())
    }
}

// ============================================================================
// Demo Scenarios
// ============================================================================

/// Run demo scenarios
pub fn run_demos() {
    println!("\n{}", "#".repeat(60));
    println!("  nRF52840 Smartwatch AI Agent - Demo Scenarios");
    println!("{}", "#".repeat(60));

    let mut agent = SmartwatchAgent::with_defaults();
    agent.config.verbose = true;

    // Demo 1: Health Check
    println!("\n\n{}", "-".repeat(60));
    println!("  Demo 1: Health Monitoring");
    println!("{}", "-".repeat(60));
    match agent.run("Check my health: read heart rate, SpO2, and send a report via BLE") {
        Ok(result) => println!("\n[RESULT] {}\n", result),
        Err(e) => println!("\n[ERROR] {}\n", e),
    }

    // Demo 2: Temperature Check with Action
    println!("\n{}", "-".repeat(60));
    println!("  Demo 2: Temperature Monitor with Fan Control");
    println!("{}", "-".repeat(60));
    match agent.run("Monitor temperature, and if it's above 25°C turn on the cooling fan") {
        Ok(result) => println!("\n[RESULT] {}\n", result),
        Err(e) => println!("\n[ERROR] {}\n", e),
    }

    // Demo 3: LED Control
    println!("\n{}", "-".repeat(60));
    println!("  Demo 3: LED Notification Control");
    println!("{}", "-".repeat(60));
    match agent.run("Turn on the LED notification light") {
        Ok(result) => println!("\n[RESULT] {}\n", result),
        Err(e) => println!("\n[ERROR] {}\n", e),
    }

    // Demo 4: Activity Tracking
    println!("\n{}", "-".repeat(60));
    println!("  Demo 4: Activity Tracking & Logging");
    println!("{}", "-".repeat(60));
    match agent.run("Track my activity: count steps and log to flash memory") {
        Ok(result) => println!("\n[RESULT] {}\n", result),
        Err(e) => println!("\n[ERROR] {}\n", e),
    }

    // Demo 5: BLE Notification
    println!("\n{}", "-".repeat(60));
    println!("  Demo 5: BLE Alert Notification");
    println!("{}", "-".repeat(60));
    match agent.run("Send a reminder notification via BLE") {
        Ok(result) => println!("\n[RESULT] {}\n", result),
        Err(e) => println!("\n[ERROR] {}\n", e),
    }

    // Final Status
    println!("\n{}", "=".repeat(60));
    println!("  Final System Status");
    println!("{}", "=".repeat(60));
    let health = agent.get_health_data();
    let info = agent.get_system_info();

    println!("\n  Battery: {}% ({}mV)", health.battery.percentage, health.battery.voltage_mv);
    println!("  Steps: {}", health.steps.steps);
    println!("  Heart Rate: {} bpm", health.heart_rate.rate);
    println!("  SpO2: {:.1}%", health.spo2.saturation);
    println!("  Temperature: {:.1}°C", health.temperature);
    println!("  Uptime: {} seconds", info.uptime_seconds);
    println!("  BLE State: {:?}", info.ble_state);
    println!("  Power Mode: {:?}", info.power_mode);
    println!("\n  Total Iterations: {}", agent.iteration());
    println!("  Total Tool Calls: {}", agent.tool_call_count);
    println!("\n{}", "=".repeat(60));
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_creation() {
        let agent = SmartwatchAgent::with_defaults();
        assert_eq!(agent.state(), AgentState::Idle);
    }

    #[test]
    fn test_health_check_task() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;

        let result = agent.run("Check my heart rate");
        assert!(result.is_ok());
    }

    #[test]
    fn test_temperature_task() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;

        let result = agent.run("Read temperature sensor");
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

        let result = agent.run("Send a notification via BLE");
        assert!(result.is_ok());
    }

    #[test]
    fn test_flash_storage() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;

        let result = agent.run("Log data to flash memory");
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
    fn test_status_check() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;

        let result = agent.run("Get system status");
        assert!(result.is_ok());
    }

    #[test]
    fn test_health_data_retrieval() {
        let agent = SmartwatchAgent::with_defaults();
        let health = agent.get_health_data();

        assert!(health.heart_rate.rate >= 50);
        assert!(health.spo2.saturation >= 90.0);
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
        assert!(agent.tool_call_count() > 0);
    }

    #[test]
    fn test_simulator_integration() {
        let mut agent = SmartwatchAgent::with_defaults();

        // Manually interact with simulator
        let result = agent.simulator_mut().execute_tool(
            "read_sensor",
            &[("sensor", "temperature")]
        );
        assert!(result.is_ok());

        let result = agent.simulator_mut().execute_tool(
            "write_gpio",
            &[("pin", "13"), ("state", "high")]
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_thread_safe_agent() {
        let agent = ThreadSafeAgent::with_defaults();

        let result = agent.run("Check battery status");
        assert!(result.is_ok());
    }

    #[test]
    fn test_complex_health_monitoring() {
        let mut agent = SmartwatchAgent::with_defaults();
        agent.config.verbose = false;

        let result = agent.run("Monitor my health: check heart rate, spo2, and battery");
        assert!(result.is_ok());

        let iterations = agent.iteration();
        let tool_calls = agent.tool_call_count();

        assert!(iterations > 0);
        assert!(tool_calls >= 2);
    }

    #[test]
    fn test_power_mode_control() {
        let mut agent = SmartwatchAgent::with_defaults();

        agent.simulator_mut().set_power_mode(PowerMode::LowPower);
        assert_eq!(agent.simulator().power_mode, PowerMode::LowPower);

        agent.simulator_mut().set_power_mode(PowerMode::Active);
        assert_eq!(agent.simulator().power_mode, PowerMode::Active);
    }
}

// ============================================================================
// Main Entry Point
// ============================================================================

fn main() {
    run_demos();
}

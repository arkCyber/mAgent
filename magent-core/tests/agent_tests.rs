//! mAgent Real Agent Tests
//!
//! Comprehensive tests for the mAgent embedded AI agent.
//! Uses simulated hardware and LLM responses for testing without real hardware.
//!
//! Run with: cargo test -p magent-core --features std --test agent_tests

#![cfg(feature = "std")]

use magent_core::agent_runner::{
    AgentState, RealAgentRunner, RunnerConfig, SamplingParams, ToolExecutor,
};
use magent_core::real_tools::SimulatorExecutor;

/// Test executor that wraps the simulator
struct TestExecutor {
    simulator: SimulatorExecutor,
}

impl TestExecutor {
    fn new() -> Self {
        let mut sim = SimulatorExecutor::new();
        sim.connect_ble();
        Self { simulator: sim }
    }
}

impl ToolExecutor for TestExecutor {
    fn execute(&mut self, tool: &str, args: &str) -> std::result::Result<String, String> {
        self.simulator.execute(tool, args)
    }
}

#[test]
fn test_agent_initialization() {
    let executor = TestExecutor::new();
    let runner = RealAgentRunner::new(executor);

    assert_eq!(runner.state(), AgentState::Idle);
    assert_eq!(runner.iteration(), 0);
    assert_eq!(runner.tool_call_count(), 0);
}

#[test]
fn test_read_temperature_sensor() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: true,
            ..Default::default()
        },
    );

    let result = runner.run("Read the temperature sensor");
    assert!(result.is_ok());

    let output = result.unwrap();
    assert!(!output.is_empty());
    println!("Temperature task result: {}", output);
}

#[test]
fn test_read_humidity_sensor() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: true,
            ..Default::default()
        },
    );

    let result = runner.run("Read the humidity sensor");
    assert!(result.is_ok());

    let output = result.unwrap();
    println!("Humidity task result: {}", output);
}

#[test]
fn test_read_pressure_sensor() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: true,
            ..Default::default()
        },
    );

    let result = runner.run("Read the pressure sensor");
    assert!(result.is_ok());

    let output = result.unwrap();
    println!("Pressure task result: {}", output);
}

#[test]
fn test_read_accelerometer() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: true,
            ..Default::default()
        },
    );

    let result = runner.run("Read the accelerometer");
    assert!(result.is_ok());

    let output = result.unwrap();
    println!("Accelerometer task result: {}", output);
}

#[test]
fn test_control_led_on() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: true,
            ..Default::default()
        },
    );

    let result = runner.run("Turn on the LED");
    assert!(result.is_ok());

    let output = result.unwrap();
    println!("LED on task result: {}", output);
}

#[test]
fn test_control_led_off() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: true,
            ..Default::default()
        },
    );

    let result = runner.run("Turn off the LED");
    assert!(result.is_ok());

    let output = result.unwrap();
    println!("LED off task result: {}", output);
}

#[test]
fn test_send_ble_message() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: true,
            ..Default::default()
        },
    );

    let result = runner.run("Send a hello message via BLE");
    assert!(result.is_ok());

    let output = result.unwrap();
    println!("BLE send task result: {}", output);
}

#[test]
fn test_flash_operations() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: true,
            ..Default::default()
        },
    );

    let result = runner.run("Write config data to flash memory");
    assert!(result.is_ok());

    let output = result.unwrap();
    println!("Flash write task result: {}", output);
}

#[test]
fn test_iteration_limit() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            max_iterations: 2,
            verbose: false,
            ..Default::default()
        },
    );

    let result = runner.run("Read temperature sensor");
    assert!(result.is_ok());
    assert!(runner.iteration() <= 2);
}

#[test]
fn test_multiple_sensor_reads() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: false,
            max_tool_calls: 3,
            ..Default::default()
        },
    );

    let result = runner.run("Monitor all sensors");
    assert!(result.is_ok());

    let output = result.unwrap();
    println!("Multi-sensor task result: {}", output);
}

#[test]
fn test_agent_message_history() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::new(executor);

    let result = runner.run("Read temperature");
    assert!(result.is_ok());

    let messages = runner.messages();
    assert!(!messages.is_empty());
}

#[test]
fn test_smart_home_scenario() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: true,
            max_iterations: 15,
            max_tool_calls: 10,
            sampling: SamplingParams::default(),
            probe_ollama_on_run: true,
            system_prompt: r#"You are mAgent, an aerospace-grade embedded AI agent.

You have access to tools:
- read_sensor(sensor): temperature, accelerometer, humidity, pressure, light
- write_gpio(pin, state): Control GPIO (high/low)
- flash_read(address): Read from flash
- flash_write(address, data): Write to flash
- ble_send(data): Send via BLE

Think step by step. When done, respond with {"result": "..."}"#
                .to_string(),
            ..Default::default()
        },
    );

    let scenario = "Check temperature, and if it's above 25C, turn on the fan (GPIO pin 14)";

    let result = runner.run(scenario);
    assert!(result.is_ok());

    let output = result.unwrap();
    println!("Smart home scenario result: {}", output);
}

#[test]
fn test_environmental_monitoring() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: true,
            max_tool_calls: 5,
            ..Default::default()
        },
    );

    let result = runner.run("Monitor the environment: read temperature, humidity, and pressure, then send the data via BLE");
    assert!(result.is_ok());

    let output = result.unwrap();
    println!("Environmental monitoring result: {}", output);
}

#[test]
fn test_health_check() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: true,
            ..Default::default()
        },
    );

    let result = runner.run(
        "Perform a system health check: verify all sensors are working and send status via BLE",
    );
    assert!(result.is_ok());

    let output = result.unwrap();
    println!("Health check result: {}", output);
}

#[test]
fn test_wearable_device_scenario() {
    let executor = TestExecutor::new();
    let mut runner = RealAgentRunner::with_config(
        executor,
        RunnerConfig {
            verbose: true,
            max_iterations: 20,
            max_tool_calls: 8,
            sampling: SamplingParams::default(),
            probe_ollama_on_run: true,
            system_prompt: r#"You are mAgent, an aerospace-grade AI agent running on a smartwatch.

You have sensors:
- Heart rate monitor
- Temperature sensor
- Accelerometer (for step counting)
- SpO2 sensor

You can:
- Control LEDs for notifications
- Vibrate the motor (GPIO)
- Store data in flash
- Send alerts via BLE

Tasks:
1. Monitor vital signs
2. Detect falls using accelerometer
3. Send emergency alerts if needed
4. Log activity to flash

Always prioritize user safety and respond quickly to emergencies."#
                .to_string(),
            ..Default::default()
        },
    );

    let scenario = "The user just woke up. Read all their vital signs, log the morning reading to flash, and send a good morning notification via BLE.";

    let result = runner.run(scenario);
    assert!(result.is_ok());

    let output = result.unwrap();
    println!("Wearable device scenario result: {}", output);
}

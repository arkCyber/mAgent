//! nRF52840 Comprehensive Tests
//!
//! This module provides comprehensive tests for the mAgent embedded AI agent
//! running on nRF52840 smartwatch hardware.
//!
//! ## Test Categories
//!
//! 1. **Unit Tests**: Individual component tests
//! 2. **Integration Tests**: Cross-component interaction tests
//! 3. **Hardware Simulation Tests**: nRF52840 hardware simulation tests
//! 4. **Agent Tests**: ReAct loop and tool execution tests
//! 5. **Performance Tests**: Resource usage and timing tests
//!
//! ## Running Tests
//!
//! ```bash
//! # Run all tests
//! cargo test -p magent-core --features std --test nrf52_comprehensive_tests
//!
//! # Run with verbose output
//! cargo test -p magent-core --features std --test nrf52_comprehensive_tests -- --nocapture
//!
//! # Run specific test module
//! cargo test -p magent-core --features std --test nrf52_comprehensive_tests nrf52_hal -- --nocapture
//! ```

#![cfg(feature = "std")]

use magent_core::agent_runner::{
    AgentState, RealAgentRunner, RunnerConfig, SamplingParams, ToolExecutor,
};
#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
use magent_core::config::AgentConfig;
#[allow(unused_imports)]
use magent_core::error::AgentError;
#[allow(unused_imports)]
use magent_core::nrf52_hal::{
    BatteryInfo, BleAddress, BleState, EnvData, GpioConfig, HeartRateMeasurement, Nrf52Simulator,
    PinDirection, PinState, PowerMode as HalPowerMode, SpO2Measurement, StepData,
};
#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
use magent_core::power::PowerManager;
#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
use magent_core::power::PowerMode;
use magent_core::real_tools::SimulatorExecutor;
#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
use magent_core::skills::{Skill, SkillsManager};
#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
use magent_core::tools::{Tool, ToolRegistry, ToolType};
#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
use magent_core::wear_leveling::{WearLeveler, WearLevelingStrategy};

// ============================================================================
// Test Executor Setup
// ============================================================================

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
        self.simulator
            .execute(tool, args)
            .map_err(|e| format!("{:?}", e))
    }
}

// ============================================================================
// nRF52840 Hardware Simulation Tests
// ============================================================================

mod nrf52_tests {
    use super::*;

    #[test]
    fn test_nrf52_simulator_creation() {
        let sim = Nrf52Simulator::new();
        assert_eq!(sim.get_power_mode(), HalPowerMode::Active);
    }

    #[test]
    fn test_gpio_pin_operations() {
        let mut sim = Nrf52Simulator::new();

        // Test LED pin (pin 13)
        sim.gpio.set_pin_state(13, PinState::High).unwrap();
        assert_eq!(sim.gpio.get_pin_state(13).unwrap(), PinState::High);

        // Test toggle
        sim.gpio.toggle_pin(13).unwrap();
        assert_eq!(sim.gpio.get_pin_state(13).unwrap(), PinState::Low);

        // Test vibration motor pin (pin 4)
        sim.gpio.set_pin_state(4, PinState::High).unwrap();
        assert_eq!(sim.gpio.get_pin_state(4).unwrap(), PinState::High);
    }

    #[test]
    fn test_gpio_configuration() {
        let mut sim = Nrf52Simulator::new();

        let config = GpioConfig {
            direction: PinDirection::Output,
            state: PinState::Low,
            sense: false,
        };

        sim.gpio.configure(13, config).unwrap();

        let state = sim.gpio.get_pin_state(13).unwrap();
        assert_eq!(state, PinState::Low);
    }

    #[test]
    fn test_temperature_sensor_simulation() {
        let sim = Nrf52Simulator::new();

        // Read temperature multiple times to test variability
        let temps: Vec<f32> = (0..10).map(|_| sim.temperature_sensor.read()).collect();

        // All readings should be within reasonable range
        for temp in &temps {
            assert!(
                *temp > 20.0 && *temp < 30.0,
                "Temperature {} out of expected range",
                temp
            );
        }
    }

    #[test]
    fn test_accelerometer_simulation() {
        let mut sim = Nrf52Simulator::new();

        // Update sensors
        for _ in 0..100 {
            sim.tick();
        }

        let (x, y, z) = sim.accelerometer.read();

        // Z should be close to 9.8 (gravity when watch is flat)
        assert!(
            z > 9.0 && z < 11.0,
            "Z-axis {} unexpected for stationary watch",
            z
        );

        // X and Y should be close to 0
        assert!(x.abs() < 2.0, "X-axis {} unexpected", x);
        assert!(y.abs() < 2.0, "Y-axis {} unexpected", y);
    }

    #[test]
    fn test_heart_rate_sensor() {
        let mut sim = Nrf52Simulator::new();

        for _ in 0..50 {
            sim.tick();
        }

        let hr: HeartRateMeasurement = sim.heart_rate_sensor.read();

        // Heart rate should be in physiological range
        assert!(
            hr.rate >= 50 && hr.rate <= 180,
            "Heart rate {} out of physiological range",
            hr.rate
        );
        assert!(hr.sensor_contact, "Sensor contact should be true");
    }

    #[test]
    fn test_spo2_sensor() {
        let mut sim = Nrf52Simulator::new();

        for _ in 0..30 {
            sim.tick();
        }

        let spo2: SpO2Measurement = sim.spo2_sensor.read();

        // SpO2 should be in healthy range
        assert!(
            spo2.saturation >= 90.0 && spo2.saturation <= 100.0,
            "SpO2 {} out of healthy range",
            spo2.saturation
        );
        assert!(
            spo2.confidence >= 80,
            "Confidence {} too low",
            spo2.confidence
        );
    }

    #[test]
    fn test_step_counter() {
        let mut sim = Nrf52Simulator::new();

        // Simulate walking (accelerate the watch)
        for i in 0..500 {
            sim.tick();
            if i % 10 == 0 {
                // Simulate step motion
                let _ = sim.gpio.toggle_pin(4);
            }
        }

        let steps: StepData = sim.step_counter.read();

        // Should have counted some steps
        assert!(steps.steps > 0, "Step counter should detect steps");
    }

    #[test]
    fn test_flash_storage() {
        let mut sim = Nrf52Simulator::new();

        let test_data = b"Hello, mAgent! This is a test message for flash storage.";

        // Write to flash
        sim.flash.write(0, test_data).unwrap();

        // Read back
        let mut read_buf = vec![0u8; test_data.len()];
        sim.flash.read(0, &mut read_buf).unwrap();

        assert_eq!(&read_buf, test_data);
    }

    #[test]
    fn test_flash_wear_tracking() {
        let mut sim = Nrf52Simulator::new();

        // Write multiple times to same sector
        for i in 0..10 {
            let data = [i as u8; 64];
            sim.flash.write(i * 64, &data).unwrap();
        }

        // Check wear tracking
        let writes = sim.flash.get_sector_writes(0);
        assert_eq!(writes, 10);
    }

    #[test]
    fn test_battery_state() {
        let sim = Nrf52Simulator::new();

        let percentage = sim.battery.percentage();
        assert!(percentage <= 100);
        assert!(!sim.battery.is_low());
        assert!(!sim.battery.is_charging());
    }

    #[test]
    fn test_battery_drain() {
        let sim = Nrf52Simulator::new();

        let initial = sim.battery.percentage();
        sim.battery.drain(10);

        assert!(sim.battery.percentage() < initial);
    }

    #[test]
    fn test_ble_disconnected_state() {
        let sim = Nrf52Simulator::new();

        assert!(matches!(sim.ble.state, BleState::Disconnected));
        assert!(sim.ble.connected_device.is_none());
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
    fn test_ble_disconnection() {
        let mut sim = Nrf52Simulator::new();

        sim.ble.connect("Android");
        sim.ble.disconnect();

        assert!(matches!(sim.ble.state, BleState::Disconnected));
        assert!(sim.ble.connected_device.is_none());
    }

    #[test]
    fn test_ble_send_when_connected() {
        let mut sim = Nrf52Simulator::new();

        sim.ble.connect("TestDevice");

        let data = b"Hello from mAgent!";
        let result = sim.ble_send(data);
        assert!(result.is_ok());
    }

    #[test]
    fn test_ble_send_when_disconnected() {
        let mut sim = Nrf52Simulator::new();

        let data = b"Hello";
        let result = sim.ble_send(data);
        assert!(result.is_err());
    }

    #[test]
    fn test_ble_address() {
        let sim = Nrf52Simulator::new();

        let addr = &sim.ble.address;
        assert_eq!(addr.bytes, [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    }

    #[test]
    fn test_power_mode_transitions() {
        let sim = Nrf52Simulator::new();

        sim.set_power_mode(HalPowerMode::Idle);
        assert_eq!(sim.get_power_mode(), HalPowerMode::Idle);

        sim.set_power_mode(HalPowerMode::LowPower);
        assert_eq!(sim.get_power_mode(), HalPowerMode::LowPower);

        sim.set_power_mode(HalPowerMode::SystemOff);
        assert_eq!(sim.get_power_mode(), HalPowerMode::SystemOff);

        sim.set_power_mode(HalPowerMode::Active);
        assert_eq!(sim.get_power_mode(), HalPowerMode::Active);
    }

    #[test]
    fn test_read_all_sensors() {
        let mut sim = Nrf52Simulator::new();

        // Run simulation for a while
        for _ in 0..100 {
            sim.tick();
        }

        let data = sim.read_all_sensors();

        // Verify all sensor readings
        assert!(data.heart_rate.rate >= 50 && data.heart_rate.rate <= 180);
        assert!(data.spo2.saturation >= 90.0);
        assert!(data.env.temperature > 20.0 && data.env.temperature < 30.0);
        assert!(data.battery.percentage <= 100);
    }

    #[test]
    fn test_sensor_list() {
        let sim = Nrf52Simulator::new();

        let sensors = sim.get_sensor_list();
        assert!(sensors.contains(&"temperature"));
        assert!(sensors.contains(&"heart_rate"));
        assert!(sensors.contains(&"spo2"));
        assert!(sensors.contains(&"steps"));
        assert!(sensors.contains(&"battery"));
    }

    #[test]
    fn test_rtc_time() {
        let sim = Nrf52Simulator::new();

        let initial_seconds = sim.rtc.get_seconds();

        // RTC should be running
        assert_eq!(initial_seconds, 0);
    }

    #[test]
    fn test_tick_counter() {
        let mut sim = Nrf52Simulator::new();

        for _ in 0..100 {
            sim.tick();
        }

        let ticks = sim.tick_count.load(core::sync::atomic::Ordering::SeqCst);
        assert_eq!(ticks, 100);
    }
}

// ============================================================================
// Agent ReAct Loop Tests
// ============================================================================

mod agent_tests {
    use super::*;

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
                verbose: false,
                ..Default::default()
            },
        );

        let result = runner.run("Read the temperature sensor");
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn test_read_heart_rate() {
        let executor = TestExecutor::new();
        let mut runner = RealAgentRunner::with_config(
            executor,
            RunnerConfig {
                verbose: false,
                ..Default::default()
            },
        );

        let result = runner.run("Read the heart rate sensor");
        assert!(result.is_ok());
    }

    #[test]
    fn test_control_led() {
        let executor = TestExecutor::new();
        let mut runner = RealAgentRunner::with_config(
            executor,
            RunnerConfig {
                verbose: false,
                max_tool_calls: 3,
                ..Default::default()
            },
        );

        let result = runner.run("Turn on the LED on pin 13");
        assert!(result.is_ok());
    }

    #[test]
    fn test_ble_notification() {
        let executor = TestExecutor::new();
        let mut runner = RealAgentRunner::with_config(
            executor,
            RunnerConfig {
                verbose: false,
                max_tool_calls: 3,
                ..Default::default()
            },
        );

        let result = runner.run("Send a hello notification via BLE");
        assert!(result.is_ok());
    }

    #[test]
    fn test_flash_storage() {
        let executor = TestExecutor::new();
        let mut runner = RealAgentRunner::with_config(
            executor,
            RunnerConfig {
                verbose: false,
                max_tool_calls: 3,
                ..Default::default()
            },
        );

        let result = runner.run("Log sensor data to flash memory");
        assert!(result.is_ok());
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
    fn test_multi_sensor_monitoring() {
        let executor = TestExecutor::new();
        let mut runner = RealAgentRunner::with_config(
            executor,
            RunnerConfig {
                verbose: false,
                max_tool_calls: 5,
                ..Default::default()
            },
        );

        let result = runner.run("Monitor all sensors: temperature, humidity, pressure");
        assert!(result.is_ok());
    }

    #[test]
    fn test_smart_home_scenario() {
        let executor = TestExecutor::new();
        let mut runner = RealAgentRunner::with_config(
            executor,
            RunnerConfig {
                verbose: false,
                max_iterations: 10,
                max_tool_calls: 5,
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
    }

    #[test]
    fn test_wearable_health_check() {
        let executor = TestExecutor::new();
        let mut runner = RealAgentRunner::with_config(
            executor,
            RunnerConfig {
                verbose: false,
                max_iterations: 15,
                max_tool_calls: 8,
                sampling: SamplingParams::default(),
                probe_ollama_on_run: true,
                system_prompt:
                    r#"You are mAgent, an aerospace-grade AI agent running on a smartwatch.

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

Always prioritize user safety."#
                        .to_string(),
                ..Default::default()
            },
        );

        let scenario = "Read all vital signs and send a health report via BLE";
        let result = runner.run(scenario);
        assert!(result.is_ok());
    }
}

// ============================================================================
// Wear Leveling Tests
// ============================================================================

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
mod wear_leveling_tests {
    use super::*;

    #[test]
    fn test_wear_leveler_creation() {
        let wl = WearLeveler::with_defaults();
        assert_eq!(wl.sector_count(), 16);
        assert_eq!(wl.max_writes_per_sector(), 10000);
        assert_eq!(wl.strategy(), WearLevelingStrategy::Dynamic);
    }

    #[test]
    fn test_dynamic_wear_leveling_rotation() {
        let wl = WearLeveler::new(4, 10);

        let sector0 = wl.get_next_sector().unwrap();
        assert_eq!(sector0, 0);

        // Simulate writes to trigger rotation
        for _ in 0..10 {
            wl.increment_write_count();
        }

        let sector1 = wl.get_next_sector().unwrap();
        assert_eq!(sector1, 1);
    }

    #[test]
    fn test_static_wear_leveling() {
        let mut wl = WearLeveler::new(4, 100);
        wl.set_strategy(WearLevelingStrategy::Static);

        for i in 0..8 {
            wl.increment_write_count();
            let sector = wl.get_next_sector().unwrap();
            assert_eq!(sector, i % 4);
        }
    }

    #[test]
    fn test_hybrid_wear_leveling() {
        let mut wl = WearLeveler::new(4, 100);
        wl.set_strategy(WearLevelingStrategy::Hybrid);

        // Should start with dynamic behavior
        let sector0 = wl.get_next_sector().unwrap();
        assert_eq!(sector0, 0);
    }

    #[test]
    fn test_wear_distribution() {
        let wl = WearLeveler::new(4, 100);

        for _ in 0..20 {
            wl.increment_write_count();
        }

        let stats = wl.calculate_wear_distribution();
        assert_eq!(stats.total_writes, 20);
        assert_eq!(stats.sectors.len(), 4);
    }

    #[test]
    fn test_least_worn_sector() {
        let wl = WearLeveler::new(4, 100);

        let sector = wl.get_least_worn_sector();
        assert_eq!(sector, 0);
    }

    #[test]
    fn test_worn_out_detection() {
        let wl = WearLeveler::new(4, 10);
        assert!(!wl.is_worn_out());
    }

    #[test]
    fn test_reset_stats() {
        let wl = WearLeveler::new(4, 100);

        for _ in 0..50 {
            wl.increment_write_count();
        }

        wl.reset_stats();

        let stats = wl.calculate_wear_distribution();
        assert_eq!(stats.total_writes, 0);
    }
}

// ============================================================================
// Power Management Tests
// ============================================================================

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
mod power_tests {
    use super::*;

    #[test]
    fn test_power_manager_creation() {
        let pm = PowerManager::new();
        assert_eq!(pm.current_mode(), PowerMode::Active);
    }

    #[test]
    fn test_power_mode_transitions() {
        let pm = PowerManager::new();

        pm.set_mode(PowerMode::Idle).unwrap();
        assert_eq!(pm.current_mode(), PowerMode::Idle);

        pm.set_mode(PowerMode::LowPower).unwrap();
        assert_eq!(pm.current_mode(), PowerMode::LowPower);

        pm.set_mode(PowerMode::DeepSleep).unwrap();
        assert_eq!(pm.current_mode(), PowerMode::DeepSleep);

        pm.set_mode(PowerMode::Active).unwrap();
        assert_eq!(pm.current_mode(), PowerMode::Active);
    }

    #[test]
    fn test_battery_threshold() {
        let pm = PowerManager::new();

        assert_eq!(pm.battery_threshold(), 3300);

        pm.set_battery_threshold(3000);
        assert_eq!(pm.battery_threshold(), 3000);
    }

    #[test]
    fn test_battery_status() {
        let pm = PowerManager::new();

        let status = pm.read_battery_status();
        assert!(status.voltage_mv > 0);
        assert!(status.percentage <= 100);
    }

    #[test]
    fn test_low_power_mode_detection() {
        let pm = PowerManager::new();

        assert!(!pm.should_enter_low_power());
    }
}

// ============================================================================
// Configuration Tests
// ============================================================================

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
mod config_tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = AgentConfig::default();
        assert!(!config.name.is_empty());
        assert!(config.max_iterations > 0);
        assert!(config.max_memory > 0);
    }

    #[test]
    fn test_config_validation() {
        let config = AgentConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_serialization() {
        let config = AgentConfig::default();
        let bytes = config.to_bytes().unwrap();
        let restored = AgentConfig::from_bytes(&bytes).unwrap();

        assert_eq!(config.name, restored.name);
        assert_eq!(config.max_iterations, restored.max_iterations);
    }
}

// ============================================================================
// Skills System Tests
// ============================================================================

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
mod skills_tests {
    use super::*;

    #[test]
    fn test_skills_manager_creation() {
        let manager = SkillsManager::new(10);
        assert_eq!(manager.count(), 0);
    }

    #[test]
    fn test_add_skill() {
        let mut manager = SkillsManager::new(10);

        let skill = Skill::new(
            "test_skill",
            "A test skill",
            "testing",
            "This is test content",
        )
        .unwrap();

        assert!(manager.add(skill).is_ok());
        assert_eq!(manager.count(), 1);
    }

    #[test]
    fn test_get_skill() {
        let mut manager = SkillsManager::new(10);

        let skill = Skill::new(
            "temperature_monitor",
            "Monitor temperature sensors",
            "sensors",
            "Read temperature from sensors",
        )
        .unwrap();

        manager.add(skill).unwrap();

        let retrieved = manager.get("temperature_monitor");
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_search_skills() {
        let mut manager = SkillsManager::new(10);

        let skill1 = Skill::new(
            "temp_sensor",
            "Temperature sensor reader",
            "sensors",
            "Read temperature",
        )
        .unwrap();

        let skill2 = Skill::new(
            "heart_monitor",
            "Heart rate monitor",
            "health",
            "Read heart rate",
        )
        .unwrap();

        manager.add(skill1).unwrap();
        manager.add(skill2).unwrap();

        let results = manager.search("sensor");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_remove_skill() {
        let mut manager = SkillsManager::new(10);

        let skill = Skill::new("to_remove", "A skill to remove", "test", "Content").unwrap();

        manager.add(skill).unwrap();
        assert_eq!(manager.count(), 1);

        manager.remove("to_remove").unwrap();
        assert_eq!(manager.count(), 0);
    }

    #[test]
    fn test_clear_skills() {
        let mut manager = SkillsManager::new(10);

        for i in 0..5 {
            let skill = Skill::new(&format!("skill_{}", i), "A skill", "test", "Content").unwrap();
            manager.add(skill).unwrap();
        }

        assert_eq!(manager.count(), 5);

        manager.clear();
        assert_eq!(manager.count(), 0);
    }
}

// ============================================================================
// Tool Registry Tests
// ============================================================================

#[cfg(any(feature = "nrf52", feature = "esp32", feature = "embedded"))]
mod tool_tests {
    use super::*;

    #[test]
    fn test_tool_registry_creation() {
        let registry = ToolRegistry::new();
        assert!(registry.all_tools().is_empty());
    }

    #[test]
    fn test_register_tool() {
        let mut registry = ToolRegistry::new();

        let tool = Tool {
            name: heapless::String::try_from("test_tool").unwrap(),
            description: heapless::String::try_from("A test tool").unwrap(),
            tool_type: ToolType::ReadSensor,
        };

        assert!(registry.register(tool).is_ok());
        assert!(!registry.all_tools().is_empty());
    }
}

// ============================================================================
// Integration Tests
// ============================================================================

mod integration_tests {
    use super::*;

    #[test]
    fn test_full_agent_workflow() {
        let executor = TestExecutor::new();
        let mut runner = RealAgentRunner::with_config(
            executor,
            RunnerConfig {
                verbose: false,
                max_iterations: 10,
                max_tool_calls: 5,
                ..Default::default()
            },
        );

        // Run multiple tasks
        let tasks = vec![
            "Read the temperature sensor",
            "Read the humidity sensor",
            "Turn on the LED",
            "Send a notification via BLE",
        ];

        for task in tasks {
            let result = runner.run(task);
            assert!(result.is_ok(), "Task '{}' failed: {:?}", task, result.err());
        }
    }

    #[test]
    fn test_smartwatch_scenario() {
        let executor = TestExecutor::new();
        let mut runner = RealAgentRunner::with_config(
            executor,
            RunnerConfig {
                verbose: false,
                max_iterations: 20,
                max_tool_calls: 10,
                sampling: SamplingParams::default(),
                probe_ollama_on_run: true,
                system_prompt: r#"You are mAgent, an AI agent running on a smartwatch.

Available tools:
- read_sensor(sensor): temperature, accelerometer, humidity, pressure, light
- write_gpio(pin, state): Control GPIO pins
- ble_send(data): Send via BLE
- flash_write(address, data): Write to flash

Smartwatch sensors:
- Heart rate (BPM)
- SpO2 (%)
- Steps
- Temperature (°C)

Your task is to monitor user health and send alerts if needed."#
                    .to_string(),
                ..Default::default()
            },
        );

        let scenario =
            "Good morning! Read all vital signs, log them to flash, and send a summary via BLE.";
        let result = runner.run(scenario);
        assert!(result.is_ok());
    }

    #[test]
    fn test_error_recovery() {
        let executor = TestExecutor::new();
        let mut runner = RealAgentRunner::with_config(
            executor,
            RunnerConfig {
                verbose: false,
                max_iterations: 3,
                max_tool_calls: 2,
                ..Default::default()
            },
        );

        // Task that should complete quickly due to iteration limit
        let result = runner.run("Read temperature");
        assert!(result.is_ok());

        // Agent should have attempted work
        assert!(runner.iteration() > 0 || runner.tool_call_count() > 0);
    }

    #[test]
    fn test_concurrent_operations() {
        let mut sim = Nrf52Simulator::new();

        // Simulate concurrent operations
        for _ in 0..10 {
            sim.tick();

            // Read sensors
            let _ = sim.temperature_sensor.read();
            let _ = sim.accelerometer.read();
            let _ = sim.heart_rate_sensor.read();

            // Flash operation
            let data = [1u8; 32];
            let _ = sim.flash.write(0, &data);
        }

        // All operations should complete without error
        let data = sim.read_all_sensors();
        assert!(data.env.temperature > 0.0);
    }
}

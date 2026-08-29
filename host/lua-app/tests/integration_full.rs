//! Full-pipeline integration + robustness tests.
//!
//! `main_lua_agent_drives_fan_via_full_pipeline` is the headline: it boots the
//! *real* `lua/main.lua` with a mock LLM backend, and asserts the script —
//! through `agent.reason()` → action → `hardware.*` — actually drives a PWM
//! fan. That is the entire "user App as brain, AI agent as brain-trust" flow in
//! one script.

#![cfg(feature = "mlua")]

use std::sync::{Arc, Mutex};

use magent_lua::action::Action;
use magent_lua::hardware::SimHardware;
use magent_lua::nvram;
use magent_lua::{install_mock_agent, HardwareBackend, LuaHostError, LuaVm, SharedHardware};

/// Boot the real `main.lua` with a mock agent that answers `SET_COOLING_PULSE:80`.
/// The script itself decides (via `agent.reason`) to set the fan PWM to 80.
#[test]
fn main_lua_agent_drives_fan_via_full_pipeline() {
    let sim: Arc<Mutex<SimHardware>> =
        Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
    let hardware: SharedHardware = sim.clone();
    let agent = install_mock_agent("SET_COOLING_PULSE:80").unwrap();
    let vm = LuaVm::new(hardware, agent).unwrap();

    vm.run_script(include_str!("../lua/main.lua")).unwrap();

    // The whole pipeline reached hardware: fan PWM was set to 80.
    assert_eq!(sim.lock().unwrap().pwm_duty(1), 80);
}

/// `main.lua` with a cool enough die should NOT call the agent / touch the fan.
#[test]
fn main_lua_stays_idle_when_cold() {
    let sim: Arc<Mutex<SimHardware>> =
        Arc::new(Mutex::new(SimHardware::default().with_temperature(25.0)));
    let hardware: SharedHardware = sim.clone();
    let agent = install_mock_agent("SET_COOLING_PULSE:80").unwrap();
    let vm = LuaVm::new(hardware, agent).unwrap();

    vm.run_script(include_str!("../lua/main.lua")).unwrap();

    let mut sim = sim.lock().unwrap();
    assert_eq!(sim.pwm_duty(1), 0, "cold die must not drive the fan");
    assert_eq!(sim.gpio_read(1).unwrap(), 0, "fan GPIO must stay off");
}

// ---------------------------------------------------------------------------
// i2c_transfer
// ---------------------------------------------------------------------------

#[test]
fn i2c_transfer_writes_then_reads() {
    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(SimHardware::default()));
    let hardware: SharedHardware = sim.clone();
    let agent = install_mock_agent("SET_COOLING").unwrap();
    let vm = LuaVm::new(hardware, agent).unwrap();

    // Write a config byte, then read back 2 bytes from the same register
    // (the common "write register, read value" I2C transaction).
    vm.run_script("local d = hardware.i2c_transfer(0x48, 0x10, 'W', 2); assert(d == 'W\\0')")
        .unwrap();
}

// ---------------------------------------------------------------------------
// Robustness: edge inputs never panic, always error or a sane value
// ---------------------------------------------------------------------------

#[test]
fn action_parse_handles_edge_inputs() {
    // Malformed / empty inputs are rejected (None), never panic.
    assert!(Action::parse("").is_none());
    assert!(Action::parse("   ").is_none());
    assert!(Action::parse(":").is_none());
    assert!(Action::parse(":80").is_none());
    assert!(Action::parse("   :").is_none());
    // A name containing ':' keeps the first split; value may itself contain ':'.
    let nested = Action::parse("a:b:c").unwrap();
    assert_eq!(nested.name, "a");
    assert_eq!(nested.value, Some("b:c"));

    // Valid forms:
    let plain = Action::parse("SET_COOLING").unwrap();
    assert_eq!(plain.name, "SET_COOLING");
    assert_eq!(plain.value, None);
    let valued = Action::parse("SET_COOLING_PULSE:80").unwrap();
    assert_eq!(valued.name, "SET_COOLING_PULSE");
    assert_eq!(valued.value, Some("80"));
}

#[test]
fn wrong_lua_arg_types_error_not_panic() {
    let hardware: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let agent = install_mock_agent("SET_COOLING").unwrap();
    let vm = LuaVm::new(hardware, agent).unwrap();

    // Passing a string where a number is expected must be a Lua error, never
    // a host panic.
    let err = vm
        .run_script("hardware.gpio_write('not-a-pin', 1)")
        .unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
    let err = vm.run_script("hardware.pwm_set(1, 'high')").unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn nvram_handles_boundary_sizes() {
    let mut hw = SimHardware::default();
    let big_key = "k".repeat(nvram::MAX_KEY_LEN);
    let big_val = "v".repeat(nvram::MAX_VALUE_LEN);
    nvram::set(&mut hw, &big_key, &big_val).unwrap();
    assert_eq!(
        nvram::get(&mut hw, &big_key).unwrap().as_deref(),
        Some(big_val.as_str())
    );

    // Overlong is rejected, not truncated silently.
    let too_key = "k".repeat(nvram::MAX_KEY_LEN + 1);
    assert!(nvram::set(&mut hw, &too_key, "v").is_err());
    let too_val = "v".repeat(nvram::MAX_VALUE_LEN + 1);
    assert!(nvram::set(&mut hw, "k", &too_val).is_err());
}

#[test]
fn nvram_many_keys_roundtrip() {
    let mut hw = SimHardware::default();
    for i in 0..50u16 {
        let k = format!("key{i}");
        let v = format!("value{i}");
        nvram::set(&mut hw, &k, &v).unwrap();
    }
    for i in 0..50u16 {
        let k = format!("key{i}");
        let v = format!("value{i}");
        assert_eq!(
            nvram::get(&mut hw, &k).unwrap().as_deref(),
            Some(v.as_str())
        );
    }
}

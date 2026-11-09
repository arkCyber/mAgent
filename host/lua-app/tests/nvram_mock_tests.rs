//! Tests for the persistent NVRAM store, the mock LLM backend, and the
//! end-to-end "agent decides → hardware acts" path.

#![cfg(feature = "mlua")]

use std::sync::{Arc, Mutex};

use magent_lua::hardware::SimHardware;
use magent_lua::nvram;
use magent_lua::runtime::AppRuntime;
use magent_lua::{install_mock_agent, LuaHostError, LuaVm, SharedHardware};

// ---------------------------------------------------------------------------
// NVRAM (Rust API, over SimHardware flash)
// ---------------------------------------------------------------------------

#[test]
fn nvram_set_get_roundtrip() {
    let mut hw = SimHardware::default();
    nvram::set(&mut hw, "mode", "fan-eco").unwrap();
    assert_eq!(
        nvram::get(&mut hw, "mode").unwrap().as_deref(),
        Some("fan-eco")
    );
}

#[test]
fn nvram_get_missing_returns_none() {
    let mut hw = SimHardware::default();
    assert_eq!(nvram::get(&mut hw, "absent").unwrap(), None);
}

#[test]
fn nvram_overwrite_replaces_previous() {
    let mut hw = SimHardware::default();
    nvram::set(&mut hw, "mode", "fan-eco").unwrap();
    nvram::set(&mut hw, "mode", "fan-full").unwrap();
    assert_eq!(
        nvram::get(&mut hw, "mode").unwrap().as_deref(),
        Some("fan-full")
    );
}

#[test]
fn nvram_preserves_other_keys_on_rewrite() {
    let mut hw = SimHardware::default();
    nvram::set(&mut hw, "mode", "fan-eco").unwrap();
    nvram::set(&mut hw, "threshold", "85").unwrap();
    // Overwriting one key must not drop the other.
    nvram::set(&mut hw, "mode", "fan-full").unwrap();
    assert_eq!(
        nvram::get(&mut hw, "mode").unwrap().as_deref(),
        Some("fan-full")
    );
    assert_eq!(
        nvram::get(&mut hw, "threshold").unwrap().as_deref(),
        Some("85")
    );
}

#[test]
fn nvram_remove_deletes_key() {
    let mut hw = SimHardware::default();
    nvram::set(&mut hw, "mode", "fan-eco").unwrap();
    nvram::remove(&mut hw, "mode").unwrap();
    assert_eq!(nvram::get(&mut hw, "mode").unwrap(), None);
}

#[test]
fn nvram_rejects_overlong_key_and_value() {
    let mut hw = SimHardware::default();
    let long_key = "k".repeat(nvram::MAX_KEY_LEN + 1);
    assert!(nvram::set(&mut hw, &long_key, "v").is_err());
    let long_val = "v".repeat(nvram::MAX_VALUE_LEN + 1);
    assert!(nvram::set(&mut hw, "k", &long_val).is_err());
}

#[test]
fn nvram_rejects_empty_key() {
    let mut hw = SimHardware::default();
    assert!(nvram::set(&mut hw, "", "v").is_err());
}

/// The device flash backend keys each exact address to its own NVS blob and
/// errors for an address that was never written. The store must treat that
/// as "empty" (not "corrupted"), and must survive a backend that returns
/// fewer bytes than requested.
#[test]
fn nvram_handles_a_keyed_blob_backend_with_normal_loads() {
    use std::collections::BTreeMap;

    /// Mimics `firmware/esp32-app/src/nvs_flash.rs`.
    struct KeyedBlobs {
        blobs: BTreeMap<u32, Vec<u8>>,
    }
    impl magent_lua::HardwareBackend for KeyedBlobs {
        fn gpio_write(&mut self, _: u8, _: u8) -> Result<(), String> {
            Ok(())
        }
        fn gpio_read(&mut self, _: u8) -> Result<u8, String> {
            Ok(0)
        }
        fn sensor_read(&mut self, _: &str) -> Result<f64, String> {
            Ok(0.0)
        }
        fn flash_read(&mut self, address: u32, _len: usize) -> Result<Vec<u8>, String> {
            match self.blobs.get(&address) {
                Some(v) => Ok(v.clone()),
                None => Err("flash_read: key not found".to_string()),
            }
        }
        fn flash_write(&mut self, address: u32, data: &[u8]) -> Result<(), String> {
            self.blobs.insert(address, data.to_vec());
            Ok(())
        }
        fn flash_erase_sector(&mut self, address: u32) -> Result<(), String> {
            self.blobs.remove(&address);
            Ok(())
        }
        fn i2c_write(&mut self, _: u8, _: u8, _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn i2c_read(&mut self, _: u8, _: u8, _: usize) -> Result<Vec<u8>, String> {
            Ok(Vec::new())
        }
        fn adc_read(&mut self, _: u8) -> Result<f64, String> {
            Ok(0.0)
        }
        fn pwm_set(&mut self, _: u8, _: u8) -> Result<(), String> {
            Ok(())
        }
        fn ble_send(&mut self, _: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn power_set(&mut self, _: u8) -> Result<(), String> {
            Ok(())
        }
    }

    let mut hw = KeyedBlobs {
        blobs: BTreeMap::new(),
    };
    // Fresh device: never-written region reads as an empty store.
    assert_eq!(nvram::get(&mut hw, "mode").unwrap(), None);
    nvram::set(&mut hw, "mode", "eco").unwrap();
    assert_eq!(nvram::get(&mut hw, "mode").unwrap().as_deref(), Some("eco"));
    nvram::set(&mut hw, "threshold", "85").unwrap();
    assert_eq!(nvram::get(&mut hw, "mode").unwrap().as_deref(), Some("eco"));
    assert_eq!(
        nvram::get(&mut hw, "threshold").unwrap().as_deref(),
        Some("85")
    );
}

// ---------------------------------------------------------------------------
// NVRAM (Lua bindings)
// ---------------------------------------------------------------------------

fn new_vm() -> LuaVm {
    let hardware: SharedHardware =
        Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
    let agent = install_mock_agent("SET_COOLING").unwrap();
    LuaVm::new(hardware, agent).unwrap()
}

#[test]
fn lua_nvram_roundtrip() {
    let vm = new_vm();
    vm.run_script("hardware.nvram_set('ssid', 'my-iot-net')")
        .unwrap();
    vm.run_script("assert(hardware.nvram_get('ssid') == 'my-iot-net')")
        .unwrap();
    vm.run_script("assert(hardware.nvram_get('nope') == nil)")
        .unwrap();
}

#[test]
fn lua_sensor_surface_covers_agent_tools() {
    let vm = new_vm();
    // The simulated sensor surface exposes the same names the `magent-core`
    // `read_sensor` tool uses, so Lua scripts exercise realistic inputs.
    vm.run_script("assert(hardware.sensor_read('temp') > 0)")
        .unwrap();
    vm.run_script("assert(hardware.sensor_read('temperature') > 0)")
        .unwrap();
    vm.run_script("assert(hardware.sensor_read('heart_rate') == 72)")
        .unwrap();
    vm.run_script("assert(hardware.sensor_read('hrv') > 0)")
        .unwrap();
    vm.run_script("assert(hardware.sensor_read('battery') > 3)")
        .unwrap();
    vm.run_script("assert(hardware.sensor_read('memory') > 0)")
        .unwrap();
    vm.run_script("assert(hardware.sensor_read('glucose') > 0)")
        .unwrap();
    // Unknown sensor → error, never a panic.
    let err = vm
        .run_script("hardware.sensor_read('does_not_exist')")
        .unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

// ---------------------------------------------------------------------------
// Mock LLM backend → agent.reason → action → hardware (end-to-end)
// ---------------------------------------------------------------------------

#[test]
fn lua_adc_reads_set_voltage() {
    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(SimHardware::default()));
    sim.lock().unwrap().set_adc(1, 3.3);
    let hardware: SharedHardware = sim.clone();
    let agent = install_mock_agent("SET_COOLING").unwrap();
    let vm = LuaVm::new(hardware, agent).unwrap();
    vm.run_script("assert(math.abs(hardware.adc_read(1) - 3.3) < 0.001)")
        .unwrap();
    // Uninitialized channel reads 0.0 V (no error).
    vm.run_script("assert(hardware.adc_read(7) == 0.0)")
        .unwrap();
}

#[test]
fn agent_decision_drives_pwm() {
    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(SimHardware::default()));
    let hardware: SharedHardware = sim.clone();
    let agent = install_mock_agent("SET_COOLING_PULSE:80").unwrap();

    let vm = LuaVm::new(hardware.clone(), agent).unwrap();
    let mut app = AppRuntime::new(vm, hardware);
    app.boot("function on_tick() return agent.reason('Device is hot','advice') end")
        .unwrap();

    let t = app.tick().unwrap();
    assert!(t.dispatched, "expected action dispatch, got {:?}", t.result);
    assert_eq!(sim.lock().unwrap().pwm_duty(2), 80);
}

#[test]
fn agent_plain_answer_is_not_dispatched() {
    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(SimHardware::default()));
    let hardware: SharedHardware = sim.clone();
    let agent = install_mock_agent("Everything is nominal.").unwrap();

    let vm = LuaVm::new(hardware.clone(), agent).unwrap();
    let mut app = AppRuntime::new(vm, hardware);
    app.boot("function on_tick() return agent.reason('status?','') end")
        .unwrap();

    let t = app.tick().unwrap();
    assert!(
        !t.dispatched,
        "prose must not be dispatched: {:?}",
        t.result
    );
}

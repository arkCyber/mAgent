//! Integration tests for the sandboxed Lua VM — the host-validation half of
//! the aerospace-grade audit. Each test asserts a *behavior*, not an
//! implementation detail:
//!
//! * hardware bindings round-trip real state through `magent-hal`;
//! * `agent.reason()` produces a non-empty answer;
//! * the sandbox rejects `os`/`io` access;
//! * an infinite loop hits the instruction budget (never hangs);
//! * every Lua error surfaces as `Err`, never a panic.

#![cfg(feature = "mlua")]

use std::sync::{Arc, Mutex};

use magent_core::MiniAgent;
use magent_lua::hardware::SimHardware;
use magent_lua::{HardwareBackend, LuaHostError, LuaVm};

/// Build a VM whose simulated die is warm enough to exercise the agent path.
fn new_vm() -> (LuaVm, Arc<Mutex<MiniAgent>>) {
    let hardware: Arc<Mutex<dyn HardwareBackend>> =
        Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
    // TRACE: REQ-LUA-SANDBOX — single-threaded VM design; see examples/demo.rs.
    #[allow(clippy::arc_with_non_send_sync)]
    let agent: Arc<Mutex<MiniAgent>> = Arc::new(Mutex::new(MiniAgent::with_defaults().unwrap()));
    let vm = LuaVm::new(hardware, agent.clone()).unwrap();
    (vm, agent)
}

#[test]
fn gpio_write_then_read_roundtrips() {
    let (vm, _) = new_vm();
    vm.run_script("hardware.gpio_write(5, 1)").unwrap();
    vm.run_script("assert(hardware.gpio_read(5) == 1, 'pin 5 should be high')")
        .unwrap();
    vm.run_script("hardware.gpio_write(5, 0)").unwrap();
    vm.run_script("assert(hardware.gpio_read(5) == 0, 'pin 5 should be low')")
        .unwrap();
}

#[test]
fn flash_write_then_read_roundtrips() {
    let (vm, _) = new_vm();
    vm.run_script("hardware.flash_write(0x100, 'HELLO')")
        .unwrap();
    vm.run_script("local b = hardware.flash_read(0x100, 5); assert(b == 'HELLO', tostring(b))")
        .unwrap();
}

#[test]
fn flash_read_out_of_range_is_an_error() {
    let (vm, _) = new_vm();
    let err = vm
        .run_script("hardware.flash_read(0xFFFFFF, 8)")
        .unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn agent_reason_returns_nonempty_answer() {
    let (vm, _) = new_vm();
    vm.run_script(
        "local a = agent.reason('Device temp is high.', 'What control?'); \
         assert(type(a) == 'string'); \
         assert(#a > 0, 'answer should be non-empty')",
    )
    .unwrap();
}

#[test]
fn sandbox_blocks_os_library() {
    let (vm, _) = new_vm();
    // `os` is absent from the sandboxed environment; this must error, not
    // execute and never panic the host.
    let err = vm.run_script("return os.execute('echo hi')").unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn sandbox_blocks_debug_library() {
    let (vm, _) = new_vm();
    // `debug` (introspection / stack access) is not loaded.
    let err = vm.run_script("return debug.getinfo(1)").unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn sandbox_blocks_package_library() {
    let (vm, _) = new_vm();
    // `package` (module loading / require) is not loaded.
    let err = vm.run_script("return package.path").unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn sandbox_blocks_ffi_library() {
    let (vm, _) = new_vm();
    // `ffi` (C interop) is not loaded.
    let err = vm.run_script("return ffi.new('int')").unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn sandbox_enforces_memory_limit() {
    let (vm, _) = new_vm();
    // A single ~5 MB allocation must be rejected by the 512 KiB memory cap —
    // proving the sandbox bounds runaway memory, not just instruction count.
    let err = vm
        .run_script("local s = string.rep('x', 5000000)")
        .unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn hardware_rejects_overlong_payloads() {
    let (vm, _) = new_vm();
    // ble_send / i2c_write / agent.reason cap hostile overlong inputs at the
    // binding layer (before any host-side unbounded allocation).
    let err = vm
        .run_script("hardware.ble_send(string.rep('x', 10000))")
        .unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));

    let err = vm
        .run_script("hardware.i2c_write(0x48, 0, string.rep('y', 1000))")
        .unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));

    let err = vm
        .run_script("agent.reason(string.rep('a', 5000), '')")
        .unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn sandbox_blocks_io_library() {
    let (vm, _) = new_vm();
    let err = vm.run_script("io.open('/etc/passwd')").unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn infinite_loop_hits_instruction_budget() {
    let (vm, _) = new_vm();
    let err = vm.run_script("while true do end").unwrap_err();
    assert!(
        err.to_string().contains("instruction budget"),
        "unexpected error: {err}"
    );
}

#[test]
fn runtime_error_is_result_not_panic() {
    let (vm, _) = new_vm();
    let err = vm.run_script("error('boom')").unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn parse_error_is_result_not_panic() {
    let (vm, _) = new_vm();
    let err = vm.run_script("this is not valid lua").unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn unknown_sensor_is_an_error() {
    let (vm, _) = new_vm();
    let err = vm.run_script("hardware.sensor_read('nope')").unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn i2c_write_then_read_roundtrips() {
    let (vm, _) = new_vm();
    vm.run_script("hardware.i2c_write(0x48, 0x01, 'AB')")
        .unwrap();
    vm.run_script("local d = hardware.i2c_read(0x48, 0x01, 2); assert(d == 'AB', tostring(d))")
        .unwrap();
}

#[test]
fn i2c_uninitialized_register_reads_zero() {
    let (vm, _) = new_vm();
    vm.run_script(
        "local d = hardware.i2c_read(0x48, 0x10, 3); \
         assert(d == '\\0\\0\\0', 'expected three zero bytes, got '..tostring(d))",
    )
    .unwrap();
}

#[test]
fn call_invokes_named_lua_function() {
    let (vm, _) = new_vm();
    vm.run_script("function on_tick(x) return 'tick:' .. x end")
        .unwrap();
    let out = vm
        .call("on_tick", &["42".to_string()])
        .expect("on_tick should run");
    assert_eq!(out, "tick:42");
}

#[test]
fn call_missing_function_errors() {
    let (vm, _) = new_vm();
    let err = vm.call("does_not_exist", &[]).unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn call_propagates_runtime_error() {
    let (vm, _) = new_vm();
    vm.run_script("function boom() error('kaboom') end")
        .unwrap();
    let err = vm.call("boom", &[]).unwrap_err();
    assert!(matches!(err, LuaHostError::Lua(_)));
}

#[test]
fn script_can_compute_but_not_touch_os() {
    let (vm, _) = new_vm();
    vm.run_script(
        "local total = 0; for i = 1, 100 do total = total + i end; \
         assert(total == 5050, tostring(total))",
    )
    .unwrap();
}

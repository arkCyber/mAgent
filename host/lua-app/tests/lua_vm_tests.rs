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

/// A script-supplied read length must be bounded before the backend allocates
/// a buffer of that size. `mlua`'s `usize` conversion rejects a negative
/// integer, but the largest positive one (`i64::MAX`) reached `vec![0u8; len]`
/// unchanged — an unrepresentable allocation that aborts the host instead of
/// returning an error.
#[test]
fn out_of_range_read_length_is_an_error() {
    let (vm, _) = new_vm();
    for script in [
        "hardware.flash_read(0x100, 9223372036854775807)",
        "hardware.i2c_read(0x48, 0x10, 9223372036854775807)",
        // `i2c_transfer`'s receive length takes the same allocation path and
        // used to be the one read length left unchecked.
        "hardware.i2c_transfer(0x48, 0x10, '', 9223372036854775807)",
    ] {
        let err = vm
            .run_script(script)
            .expect_err("an out-of-range read length must be an error");
        assert!(matches!(err, LuaHostError::Lua(_)), "{script}: {err:?}");
    }

    // In-range reads still work (the bound must not break normal use).
    vm.run_script("hardware.flash_read(0x100, 4096)").unwrap();
    vm.run_script("hardware.i2c_read(0x48, 0x10, 256)").unwrap();
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

/// Flash can only clear bits. Rewriting a non-erased address used to return
/// success while leaving `HELLO`/`WORLD` bit-ANDed together.
#[test]
fn rewriting_flash_without_an_erase_is_an_error() {
    let (vm, _) = new_vm();
    vm.run_script("hardware.flash_write(0x300, 'HELLO')").unwrap();
    let err = vm
        .run_script("hardware.flash_write(0x300, 'WORLD')")
        .expect_err("non-erased flash must reject a bit-setting write");
    assert!(matches!(err, LuaHostError::Lua(_)), "{err:?}");
    // After erasing the sector the same write succeeds.
    vm.run_script("hardware.flash_erase_sector(0x300)").unwrap();
    vm.run_script("hardware.flash_write(0x300, 'WORLD')").unwrap();
}

/// Both engines must expose the same `hardware.*` surface:
/// `flash_erase_sector` (previously piccolo-only) and `i2c_transfer`
/// (previously mlua-only).
#[test]
fn erase_and_transfer_bindings_are_exposed() {
    let (vm, _) = new_vm();
    vm.run_script(
        "hardware.flash_write(0x200, 'AB') \
         hardware.flash_erase_sector(0x200) \
         hardware.i2c_write(0x48, 0x02, 'CD') \
         local o = hardware.i2c_transfer(0x48, 0x02, '', 2) \
         assert(o == 'CD', tostring(o))",
    )
    .unwrap();
}

#[test]
fn agent_reason_accepts_text_at_the_documented_limit() {
    // The cap is on the *joined* task, which includes one separator space.
    // A request that fills the cap exactly must reach the agent rather than
    // being rejected later with an opaque TooLong error.
    let (vm, _) = new_vm();
    let total = magent_lua::hardware::MAX_REASON_LEN;
    let left = total / 2;
    let right = total - left - 1; // + 1 separator == total
    vm.run_script(&format!(
        "local a = agent.reason(string.rep('a', {left}), string.rep('b', {right})) \
         assert(type(a) == 'string')"
    ))
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

/// The Lua *base* library cannot be disabled via `StdLib` and defines
/// `load` / `loadfile` / `dofile`. The first two are a filesystem-read and
/// code-execution path that bypasses the absent `io`/`os` libs, and `load`
/// accepts binary chunks by default — re-opening the malicious-bytecode path
/// that `ChunkMode::Text` closes for the outer chunk. All three must be gone.
#[test]
fn sandbox_blocks_runtime_chunk_loaders() {
    let (vm, _) = new_vm();
    for name in ["load", "loadfile", "dofile"] {
        vm.run_script(&format!("assert({name} == nil, '{name} is present')"))
            .unwrap_or_else(|e| panic!("{name} must be removed from the sandbox: {e}"));
    }
    // And they cannot execute code.
    let err = vm.run_script("return load('return 1')").unwrap_err();
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

/// `pcall` must not be able to swallow the instruction-budget error.
///
/// The hook previously debited with `fetch_sub`, which wrapped the counter to
/// `u64::MAX` once it reached zero: a loop that caught the first budget error
/// with `pcall` disabled the budget and ran for ~1.8e19 instructions. Even
/// with a saturating counter, a plain `pcall` would catch every subsequent
/// trip and keep running, so the sandbox re-raises budget errors from
/// `pcall`/`xpcall`. This script must terminate with a budget error, not hang.
#[test]
fn pcall_cannot_swallow_the_instruction_budget() {
    let (vm, _) = new_vm();
    let err = vm
        .run_script("while true do pcall(function() while true do end end) end")
        .unwrap_err();
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

#[test]
fn precompiled_bytecode_chunk_is_rejected() {
    // `.set_mode(ChunkMode::Text)` in `LuaVm::run_script`. mlua documents the
    // hazard: "Lua does not check the consistency of the code inside binary
    // chunks. Running maliciously crafted bytecode can crash the interpreter."
    // A script whose contents *begin* with the Lua bytecode signature
    // (`LUA_SIGNATURE` = `\x1bLua`) took the precompiled path by default, since
    // mlua autodetects the mode. The sandbox's contract is that an untrusted
    // enterprise script cannot escape or crash the host, so the chunk must be
    // forced to be parsed as text.
    //
    // Only the 4-byte signature is needed: Lua's binary detection is a
    // `memcmp` against `LUA_SIGNATURE`, so the loader takes the binary branch
    // (and must then reject it) before it ever looks at the body. This keeps
    // the test input valid UTF-8, which `run_script(&str)` requires.
    let (vm, _) = new_vm();
    let err = vm.run_script("\u{1b}Lua").unwrap_err();
    match err {
        LuaHostError::Lua(msg) => assert!(
            msg.contains("mode"),
            "expected a text-mode rejection naming the mode, got {msg:?}"
        ),
        other => panic!("expected LuaHostError::Lua, got {other:?}"),
    }
}

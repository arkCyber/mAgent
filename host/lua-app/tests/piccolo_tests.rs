//! Minimal proof-of-concept: a pure-Rust `piccolo` Lua VM can host a script
//! that calls a Rust callback. This de-risks the ESP32-S3 path — `piccolo`
//! compiles for any target (no C), unlike `mlua`.
//!
//! Built only when the `piccolo` feature is on:
//! `cargo test -p magent-lua --features piccolo --test piccolo_tests`

#![cfg(feature = "piccolo")]

use piccolo::{
    Callback, CallbackReturn, Closure, Executor, Function, FunctionPrototype, Lua, Variadic,
};

/// Register a Rust `add(a, b)` global and run a Lua chunk that calls it.
#[test]
fn piccolo_runs_rust_callback() {
    let mut lua = Lua::core();

    // Register a Rust callback as the Lua global `add`.
    lua.enter(|ctx| {
        let add = Callback::from_fn(&ctx, |ctx, _exec, mut stack| {
            let a: i64 = stack.from_front(ctx)?;
            let b: i64 = stack.from_front(ctx)?;
            stack.replace(ctx, a + b);
            Ok(CallbackReturn::Return)
        });
        ctx.set_global("add", add).unwrap();
    });

    // Compile `return add(2, 3)` into a closure, start it on an executor, and
    // stash the executor so `Lua::execute` can drive it to completion.
    let stashed = lua.enter(|ctx| {
        let proto =
            FunctionPrototype::compile(ctx, "<test>", "return add(2, 3)".as_bytes()).unwrap();
        let closure = Closure::new(&ctx, proto, Some(ctx.globals())).unwrap();
        let executor = Executor::start(
            ctx,
            Function::from(closure),
            Variadic(Vec::<piccolo::Value>::new()),
        );
        ctx.stash(executor)
    });

    let result: i64 = lua.execute(&stashed).unwrap();
    assert_eq!(result, 5, "piccolo should run a Rust callback from Lua");
}

/// The `PiccoloVm` binds `hardware.*` and `agent.reason` onto the real
/// `HardwareBackend` / `SharedAgent`, so a script can drive the simulator.
#[test]
fn piccolo_vm_binds_hardware_and_agent() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, HardwareBackend, SharedHardware};
    use std::sync::{Arc, Mutex};

    let sim: Arc<Mutex<SimHardware>> =
        Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
    let hw: SharedHardware = sim.clone();
    let agent = install_mock_agent("SET_COOLING_PULSE:80").unwrap();

    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);

    // A script that reads the die temp, drives GPIO, and consults the agent —
    // the real "App as brain" shape, running on the pure-Rust engine.
    vm.run_script(
        "local temp = hardware.sensor_read('temp') \
         hardware.gpio_write(1, 1) \
         local action = agent.reason('Device is hot', 'advice') \
         assert(temp == 42.0, 'temp should be 42') \
         assert(action == 'SET_COOLING_PULSE:80', 'agent action')",
    )
    .unwrap();

    let mut sim = sim.lock().unwrap();
    assert_eq!(sim.gpio_read(1).unwrap(), 1, "GPIO written via piccolo Lua");
}

/// The `PiccoloVm` covers the full common `hardware.*` surface on the
/// pure-Rust engine, so a realistic app script drives I2C/ADC/PWM/GPIO.
#[test]
fn piccolo_vm_full_hardware_surface() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, HardwareBackend, SharedHardware};
    use std::sync::{Arc, Mutex};

    let sim: Arc<Mutex<SimHardware>> =
        Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
    sim.lock().unwrap().set_adc(1, 3.3);
    let hw: SharedHardware = sim.clone();
    let agent = install_mock_agent("SET_COOLING_PULSE:80").unwrap();

    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);
    vm.run_script(
        "local temp = hardware.sensor_read('temp') \
         hardware.gpio_write(1, 1) \
         hardware.i2c_write(0x48, 0x10, 'AB') \
         local got = hardware.i2c_read(0x48, 0x10, 2) \
         local adc = hardware.adc_read(1) \
         hardware.pwm_set(2, 80) \
         local action = agent.reason('Device is hot', 'advice') \
         assert(temp == 42.0, 'temp') \
         assert(got == 'AB', 'i2c ' .. tostring(got)) \
         assert(adc == 3.3, 'adc') \
         assert(action == 'SET_COOLING_PULSE:80', 'agent')",
    )
    .unwrap();

    let mut sim = sim.lock().unwrap();
    assert_eq!(sim.gpio_read(1).unwrap(), 1, "gpio written");
    assert_eq!(sim.pwm_duty(2), 80, "pwm set");
    assert!((sim.adc_read(1).unwrap() - 3.3).abs() < 1e-6, "adc read");
    assert_eq!(
        String::from_utf8(sim.i2c_read(0x48, 0x10, 2).unwrap()).unwrap(),
        "AB",
        "i2c roundtrip"
    );
}

/// `^`/`$` used to be consumed and ignored, so an anchored pattern matched
/// anywhere. Validation scripts that rely on anchors could be bypassed.
#[test]
fn piccolo_string_match_honours_anchors() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, SharedHardware};
    use std::sync::{Arc, Mutex};

    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(SimHardware::default()));
    let hw: SharedHardware = sim;
    let agent = install_mock_agent("ok").unwrap();
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);

    vm.run_script(
        r#"
        -- Anchored at start: 'abc' is present but not at offset 0.
        assert(string.match('xabc', '^abc') == nil, '^ must anchor')
        assert(string.match('abc', '^abc') == 'abc')
        -- Anchored at end.
        assert(string.match('abcx', 'abc$') == nil, '$ must anchor')
        assert(string.match('abc', 'abc$') == 'abc')
        -- Both, including a capture.
        assert(string.match('ab', '^(a)b$') == 'a')
        -- Unanchored matching still scans.
        assert(string.match('xabc', 'abc') == 'abc')
        "#,
    )
    .unwrap();
}

/// Integer arguments were truncated with `as u8`, so `gpio_write(256, 1)`
/// silently drove pin 0 (and `i2c_write(0x148, …)` addressed 0x48) on this
/// engine while the mlua engine rejected them. Same script must not reach
/// different hardware depending on the engine.
#[test]
fn piccolo_hardware_arguments_are_range_checked() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, HardwareBackend, SharedHardware};
    use std::sync::{Arc, Mutex};

    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(SimHardware::default()));
    let hw: SharedHardware = sim.clone();
    let agent = install_mock_agent("ok").unwrap();
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);

    for script in [
        "hardware.gpio_write(256, 1)",
        "hardware.gpio_write(1, 256)",
        "hardware.i2c_write(0x148, 0, 'x')",
        "hardware.flash_write(-1, 'x')",
        "hardware.flash_read(4294967296, 1)",
    ] {
        let err = vm.run_script(script).unwrap_err();
        assert!(
            matches!(err, magent_lua::LuaHostError::Lua(_)),
            "{script}: {err:?}"
        );
    }

    // Pin 0 must not have been driven by the rejected first script.
    assert_eq!(sim.lock().unwrap().gpio_read(0).unwrap(), 0);
}

/// `PiccoloVm::call` must convert nil/number/boolean like the mlua engine;
/// demanding a String made `on_tick` (implicit nil) fail every tick.
#[test]
fn piccolo_call_converts_non_string_returns() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, SharedHardware};
    use std::sync::{Arc, Mutex};

    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(SimHardware::default()));
    let hw: SharedHardware = sim;
    let agent = install_mock_agent("ok").unwrap();
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);
    vm.run_script(
        "function n() end function i() return 42 end \
         function b() return true end function s() return 'x' end",
    )
    .unwrap();
    assert_eq!(vm.call("n", &[]).unwrap(), "");
    assert_eq!(vm.call("i", &[]).unwrap(), "42");
    assert_eq!(vm.call("b", &[]).unwrap(), "true");
    assert_eq!(vm.call("s", &[]).unwrap(), "x");
}

/// Engine parity: `i2c_transfer` is now bound here too.
#[test]
fn piccolo_exposes_transfer_and_erase() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, SharedHardware};
    use std::sync::{Arc, Mutex};

    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(SimHardware::default()));
    let hw: SharedHardware = sim;
    let agent = install_mock_agent("ok").unwrap();
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);
    vm.run_script(
        "hardware.i2c_write(0x48, 0x02, 'CD') \
         local o = hardware.i2c_transfer(0x48, 0x02, '', 2) \
         assert(o == 'CD', tostring(o)) \
         hardware.flash_write(0x200, 'AB') \
         hardware.flash_erase_sector(0x200)",
    )
    .unwrap();
}

/// Whole-group captures, group quantifiers, escaped `$`, bounded `format`
/// precision, and a bounded `match` comparison budget.
#[test]
fn piccolo_string_patterns_match_the_mlua_semantics() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, SharedHardware};
    use std::sync::{Arc, Mutex};

    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(SimHardware::default()));
    let hw: SharedHardware = sim;
    let agent = install_mock_agent("ok").unwrap();
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);
    vm.run_script(
        r#"
        assert(string.match('ab', '(ab)') == 'ab', 'group span')
        assert(string.match('abab', '(ab)+') == 'ab', 'group quantifier')
        assert(string.match('a$b', 'a%$b') == 'a$b', 'escaped dollar')
        assert(string.match('abcx', 'abc%$') == nil, 'escaped $ is not an anchor')
        -- Precision is capped, so both render the same number of digits.
        assert(#string.format('%.50f', 1) == #string.format('%.32f', 1))
        "#,
    )
    .unwrap();

    // Pathological O(n·m) matching is bounded: a 3000-byte literal that only
    // fails at its final byte forces ~4.5M comparisons across start offsets,
    // which exceeds the 1M budget and returns nil instead of running for
    // minutes. (Building the string in Lua would trip the arena limit first.)
    let big = "a".repeat(3000);
    vm.run_script(&format!(
        "assert(string.match('{big}', '{big}' .. 'b') == nil)"
    ))
    .unwrap();
}

/// Boot the exact `DEFAULT_MAIN_LUA` self-test the S3 firmware embeds (see
/// `firmware/esp32-app/src/lua_task.rs`) through `AppRuntime<PiccoloVm>` +
/// `SimHardware`. Every wired driver (`sensor_read`, `adc_read`, `pwm_set`,
/// `i2c_read`, `gpio_write`) returns `Ok` on the simulator, so all `pcall`
/// probes pass and boot must not error — proving the shipped script's syntax
/// and `hardware.*` usage are correct on the engine the device will run.
#[test]
fn app_runtime_boots_s3_selftest_script() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::runtime::AppRuntime;
    use magent_lua::{install_mock_agent, HardwareBackend, SharedHardware};
    use std::sync::{Arc, Mutex};

    let firmware_selftest = r#"
-- default enterprise app (embedded bootstrap + hardware self-test)
local function probe(label, fn)
    local ok, res = pcall(fn)
    if ok then
        print("[lua] " .. label .. " ok  " .. tostring(res))
    else
        print("[lua] " .. label .. " err " .. tostring(res))
    end
end
probe("temp",   function() return hardware.sensor_read("temp") end)
probe("adc",    function() return hardware.adc_read(1) end)
probe("pwm",    function() hardware.pwm_set(1, 50) return "duty=50%" end)
probe("i2c",    function() return hardware.i2c_read(0x38, 0x0F, 1) end)
probe("gpio",   function() hardware.gpio_write(2, 1) return "p2=1" end)
probe("flash",  function() hardware.flash_write(0x100, "HELLO") return hardware.flash_read(0x100, 5) end)
probe("ble",    function() hardware.ble_send("lua-ok") return "sent" end)
local temp = hardware.sensor_read("temp")
if temp > 85.0 then
    hardware.gpio_write(1, 1)
end
"#;

    let sim: Arc<Mutex<SimHardware>> =
        Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
    let hw: SharedHardware = sim.clone();
    let agent = install_mock_agent("SET_COOLING").unwrap();

    let vm = magent_lua::piccolo_vm::PiccoloVm::new(hw.clone(), agent);
    let mut app = AppRuntime::new(vm, hw);
    // A boot error here would mean the shipped S3 script is invalid — fail fast.
    app.boot(firmware_selftest).unwrap();
    assert_eq!(
        sim.lock().unwrap().gpio_read(2).unwrap(),
        1,
        "selftest gpio probe ran"
    );
}

/// `PiccoloVm::call` / `has` enable the event-loop (`on_tick`) pattern on the
/// pure-Rust engine — the missing piece for driving it with `AppRuntime`.
#[test]
fn piccolo_vm_call_invokes_lua_function() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, SharedHardware};
    use std::sync::{Arc, Mutex};

    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let agent = install_mock_agent("SET_COOLING").unwrap();

    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);
    vm.run_script("function on_tick(x) return 'tick:' .. x end")
        .unwrap();

    assert!(vm.has("on_tick"), "on_tick should be defined");
    assert!(!vm.has("does_not_exist"), "missing global should be false");

    let out = vm.call("on_tick", &["42".to_string()]).unwrap();
    assert_eq!(out, "tick:42");

    let err = vm.call("missing_fn", &[]).unwrap_err();
    assert!(matches!(err, magent_lua::LuaHostError::Lua(_)));
}

/// The payoff of the engine abstraction: the FULL `AppRuntime` (tick loop,
/// action dispatch, health/watchdog) runs on the pure-Rust `piccolo` engine —
/// the ESP32-S3 path.
#[test]
fn app_runtime_drives_piccolo_engine() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::runtime::AppRuntime;
    use magent_lua::{install_mock_agent, HardwareBackend, SharedHardware};
    use std::sync::{Arc, Mutex};

    let sim: Arc<Mutex<SimHardware>> =
        Arc::new(Mutex::new(SimHardware::default().with_temperature(42.0)));
    let hw: SharedHardware = sim.clone();
    let agent = install_mock_agent("SET_COOLING_PULSE:80").unwrap();

    let vm = magent_lua::piccolo_vm::PiccoloVm::new(hw.clone(), agent);
    let mut app = AppRuntime::new(vm, hw); // AppRuntime<PiccoloVm>
    app.boot("function on_tick() return 'FAN_ON' end").unwrap();

    let t = app.tick().unwrap();
    assert!(t.dispatched, "on_tick FAN_ON should dispatch to gpio");
    assert_eq!(app.tick_count(), 1);
    assert_eq!(app.health(std::time::Duration::from_secs(1)).error_count, 0);
    assert_eq!(
        sim.lock().unwrap().gpio_read(1).unwrap(),
        1,
        "fan GPIO via piccolo AppRuntime"
    );
}

/// `PiccoloVm` now covers the FULL `HardwareBackend` surface: flash, NVRAM
/// (persistent KV), BLE, and power — so the pure-Rust engine is a drop-in for
/// the S3 app environment.
#[test]
fn piccolo_vm_covers_full_hardware_surface() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, HardwareBackend, SharedHardware};
    use std::sync::{Arc, Mutex};

    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(SimHardware::default()));
    let hw: SharedHardware = sim.clone();
    let agent = install_mock_agent("SET_COOLING").unwrap();

    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);
    vm.run_script(
        "hardware.flash_write(0x100, 'HELLO') \
         local f = hardware.flash_read(0x100, 5) \
         hardware.nvram_set('mode', 'eco') \
         local m = hardware.nvram_get('mode') \
         hardware.ble_send('payload') \
         hardware.power_set(1) \
         assert(f == 'HELLO', 'flash ' .. tostring(f)) \
         assert(m == 'eco', 'nvram ' .. tostring(m))",
    )
    .unwrap();

    let mut sim = sim.lock().unwrap();
    assert_eq!(
        String::from_utf8(sim.flash_read(0x100, 5).unwrap()).unwrap(),
        "HELLO",
        "flash roundtrip"
    );
    assert_eq!(sim.gpio_read(1).unwrap(), 0, "gpio untouched");
}

/// The piccolo engine enforces an **instruction budget**, so a hostile or buggy
/// `main.lua` (infinite loop) returns an error instead of hanging the
/// `lua-thread` forever — the core sandbox guarantee the S3 relies on. The
/// mlua sandbox's `set_memory_limit`/instruction hook does NOT apply to the
/// piccolo engine, so this is enforced in `execute_bounded` (piccolo `Fuel`).
#[test]
fn piccolo_engine_enforces_instruction_budget() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, SharedHardware};
    use std::sync::{Arc, Mutex};

    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let agent = install_mock_agent("SET_COOLING").unwrap();
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);

    // An empty infinite loop must exhaust the budget and return an error,
    // not hang the test (and not hang the device in production).
    let err = vm.run_script("while true do end").unwrap_err();
    assert!(
        matches!(err, magent_lua::LuaHostError::Lua(_)),
        "infinite loop must error, not hang: {err:?}"
    );
    assert!(
        err.to_string().contains("budget"),
        "error should mention the instruction budget: {err}"
    );

    // The same budget applies to the `call` path used by the event loop.
    vm.run_script("function on_tick() local n=0 while true do n=n+1 end return 'x' end")
        .unwrap();
    let err2 = vm.call("on_tick", &[]).unwrap_err();
    assert!(
        err2.to_string().contains("budget"),
        "on_tick runaway: {err2}"
    );

    // Sanity: a finite script still completes normally (no false positive).
    let mut vm2 = magent_lua::piccolo_vm::PiccoloVm::new(
        Arc::new(Mutex::new(SimHardware::default())),
        install_mock_agent("SET_COOLING").unwrap(),
    );
    vm2.run_script("local s=0 for i=1,1000 do s=s+i end assert(s==500500)")
        .unwrap();
}

/// The piccolo engine also enforces a **memory cap** (`Lua::total_memory`), so a
/// script that grows the heap unboundedly (not just an infinite loop) is
/// contained instead of exhausting the S3's PSRAM.
#[test]
fn piccolo_engine_enforces_memory_limit() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, SharedHardware};
    use std::sync::{Arc, Mutex};

    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let agent = install_mock_agent("SET_COOLING").unwrap();
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);

    // Grows a table past the default 512 KiB arena cap; the memory limit must
    // fire before the instruction budget (a handful of ops per iteration).
    // (The N8R8 `s3-8mb-psram` profile raises the cap to 2 MiB.)
    let err = vm
        .run_script("local t={} while #t < 4000000 do t[#t+1]='x' end")
        .unwrap_err();
    assert!(
        err.to_string().contains("memory"),
        "runaway allocation must hit the memory cap: {err}"
    );
}

/// The piccolo `hardware.*` / `agent` bindings surface **argument / backend
/// errors as Lua errors, never a panic or hang** — the error-path closures in
/// `PiccoloVm` (bad arg type, missing arg, unknown sensor, unknown callable).
#[test]
fn piccolo_vm_hardware_error_paths() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, LuaHostError, SharedHardware};
    use std::sync::{Arc, Mutex};

    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let agent = install_mock_agent("SET_COOLING").unwrap();
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);

    let is_lua_err = |e: LuaHostError| matches!(e, LuaHostError::Lua(_));

    // Wrong argument type -> Lua error, not a panic.
    assert!(
        is_lua_err(vm.run_script("hardware.gpio_write('abc', 1)").unwrap_err()),
        "non-integer pin must error"
    );
    // Missing argument -> Lua error.
    assert!(
        is_lua_err(vm.run_script("hardware.adc_read()").unwrap_err()),
        "missing arg must error"
    );
    // Unsupported sensor name -> backend Err propagated as a Lua error.
    assert!(
        is_lua_err(
            vm.run_script("hardware.sensor_read('bogus_sensor')")
                .unwrap_err()
        ),
        "unknown sensor must error"
    );
    // Wrong arg type on i2c_read -> Lua error.
    assert!(
        is_lua_err(vm.run_script("hardware.i2c_read('x', 1, 1)").unwrap_err()),
        "non-integer i2c addr must error"
    );
    // Calling an undefined global -> Lua error (not a panic).
    assert!(
        is_lua_err(vm.call("does_not_exist", &[]).unwrap_err()),
        "undefined callable must error"
    );
}

/// A script-supplied read length must be rejected, not handed to the backend's
/// allocation. A negative Lua integer wraps to a huge `usize` through the
/// `as usize` cast in the binding, and the old code passed it straight to
/// `vec![0u8; len]` / `Vec::with_capacity(len)` — a capacity overflow that
/// aborts the whole host process instead of returning the error the binding
/// promises.
#[test]
fn piccolo_vm_rejects_an_out_of_range_read_length() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, LuaHostError, SharedHardware};
    use std::sync::{Arc, Mutex};

    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let agent = install_mock_agent("SET_COOLING").unwrap();
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);

    for script in [
        // `-1` becomes `usize::MAX` under the binding's `as usize` cast.
        "hardware.flash_read(0x100, -1)",
        "hardware.i2c_read(0x48, 0x10, -1)",
        // A large positive length would otherwise drive a giant allocation.
        "hardware.flash_read(0x100, 1000000000)",
        "hardware.i2c_read(0x48, 0x10, 1000000000)",
    ] {
        let err = vm
            .run_script(script)
            .expect_err("an out-of-range read length must be an error");
        assert!(
            matches!(err, LuaHostError::Lua(_) | LuaHostError::Hardware(_)),
            "{script} should surface as a Lua/hardware error, got {err:?}"
        );
    }

    // In-range reads still work (the bound must not break normal use).
    vm.run_script(
        "hardware.flash_write(0x100, 'HELLO') \
         assert(hardware.flash_read(0x100, 5) == 'HELLO', 'flash roundtrip') \
         hardware.i2c_write(0x48, 0x10, 'AB') \
         assert(hardware.i2c_read(0x48, 0x10, 2) == 'AB', 'i2c roundtrip')",
    )
    .unwrap();
}

/// The I2C register file is addressed with an 8-bit offset, so the backend
/// computes each register as `reg.wrapping_add(off as u8)`. A payload longer
/// than 256 bytes therefore folds back onto registers it already wrote: the
/// script is told the write succeeded while the simulator state is silently
/// corrupted. The `mlua` engine rejects oversized payloads in its own binding;
/// the `piccolo` engine (what the ESP32-S3 runs) had no bound at all.
#[test]
fn piccolo_vm_rejects_an_oversized_i2c_write() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, SharedHardware};
    use std::sync::{Arc, Mutex};
    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let agent = install_mock_agent("SET_COOLING").unwrap();
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);

    // 300 bytes: offsets 256.. wrap onto 0.. and clobber the first 44 bytes.
    let big: String = std::iter::repeat_n('x', 300).collect();
    vm.run_script(&format!("hardware.i2c_write(0x48, 0x10, '{big}')"))
        .expect_err("an oversized i2c payload must be an error");

    // A payload at the bound is still accepted.
    let ok: String = std::iter::repeat_n('x', 256).collect();
    vm.run_script(&format!(
        "hardware.i2c_write(0x48, 0x10, '{ok}') \
         assert(#hardware.i2c_read(0x48, 0x10, 256) == 256, 'i2c roundtrip')"
    ))
    .unwrap();
}

/// `string.match` is a hand-written shim whose docs promise `nil` for any
/// pattern it does not support. A pattern that ends in an unterminated `(` is
/// unsupported (real Lua raises "unfinished capture"), but the shim computed
/// the capture body as `&pat[group_start..i - 1]` — for `(` at the end of the
/// pattern `i - 1` lands *before* `group_start`, and the reversed slice panics
/// the whole VM.
#[test]
fn piccolo_string_match_tolerates_an_unterminated_capture() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, SharedHardware};
    use std::sync::{Arc, Mutex};

    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let agent = install_mock_agent("SET_COOLING").unwrap();
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);

    vm.run_script(
        "assert(string.match('abc', '(') == nil, 'trailing open paren') \
         assert(string.match('abc', 'ab(') == nil, 'open paren after a prefix') \
         assert(string.match('abc', '(a(b') == nil, 'unbalanced nested group') \
         assert(string.match('abc', '(b)') == 'b', 'a closed capture still works')",
    )
    .unwrap();
}

/// A poisoned `SharedHardware` lock (e.g. a panic while a callback held the
/// `Mutex`) must surface as a Lua error through the piccolo bindings, never a
/// host panic — the lock-poisoning error paths in `PiccoloVm`.
#[test]
fn piccolo_vm_surfaces_poisoned_hardware_lock() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, LuaHostError, SharedHardware};
    use std::sync::{Arc, Mutex};

    let hw: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(SimHardware::default()));
    // Poison the mutex by panicking while it is held (on another thread so the
    // test process survives).
    {
        let hw2 = hw.clone();
        let t = std::thread::spawn(move || {
            let _guard = hw2.lock().unwrap();
            panic!("intentional poison");
        });
        let _ = t.join();
    }
    assert!(
        hw.is_poisoned(),
        "the lock should be poisoned after the panic"
    );

    let shared: SharedHardware = hw.clone();
    let agent = install_mock_agent("SET_COOLING").unwrap();
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(shared, agent);

    let err = vm.run_script("hardware.gpio_read(1)").unwrap_err();
    assert!(
        matches!(err, LuaHostError::Lua(_)),
        "poisoned lock must surface as a Lua error: {err:?}"
    );
    assert!(
        err.to_string().contains("poisoned"),
        "error should mention the poisoned lock: {err}"
    );
}

/// Hot-reload (`PiccoloVm::reload_state` / `AppRuntime::reload`): rebuilding the
/// interpreter from a clean slate must clear stale globals (so the previous app
/// cannot leak) and let a changed `main.lua` take effect without a reboot.
#[test]
fn piccolo_vm_hot_reload_swaps_script() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::piccolo_vm::PiccoloVm;
    use magent_lua::runtime::AppRuntime;
    use magent_lua::{install_mock_agent, HardwareBackend, SharedHardware};
    use std::sync::{Arc, Mutex};

    let sim: Arc<Mutex<SimHardware>> = Arc::new(Mutex::new(SimHardware::default()));
    let hw: SharedHardware = sim.clone();
    let agent = install_mock_agent("SET_COOLING").unwrap();

    // Direct VM: reload_state must clear stale globals (functions included).
    let mut vm = PiccoloVm::new(hw.clone(), agent);
    vm.run_script("function on_tick() return 'OLD' end")
        .unwrap();
    assert!(vm.has("on_tick"));
    assert_eq!(vm.call("on_tick", &[]).unwrap(), "OLD");
    vm.reload_state().unwrap();
    assert!(
        !vm.has("on_tick"),
        "reload must clear stale globals (functions too)"
    );
    vm.run_script("function on_tick() return 'NEW' end")
        .unwrap();
    assert_eq!(vm.call("on_tick", &[]).unwrap(), "NEW");

    // AppRuntime: reload drives the *changed* script through the event loop.
    let mut app = AppRuntime::new(
        PiccoloVm::new(hw.clone(), install_mock_agent("SET_COOLING").unwrap()),
        hw,
    );
    app.boot("function on_tick() hardware.gpio_write(1, 1) return '' end")
        .unwrap();
    app.tick().unwrap();
    assert_eq!(sim.lock().unwrap().gpio_read(1).unwrap(), 1);
    app.reload("function on_tick() hardware.gpio_write(1, 0) return '' end")
        .unwrap();
    assert_eq!(app.tick_count(), 0, "reload resets the tick counter");
    app.tick().unwrap();
    assert_eq!(
        sim.lock().unwrap().gpio_read(1).unwrap(),
        0,
        "the reloaded script must drive gpio"
    );
}

/// A poisoned `SharedAgent` lock must surface through `agent.reason` as a Lua
/// error (never a host panic) — the agent error-path closure in `PiccoloVm`.
#[test]
fn piccolo_vm_surfaces_poisoned_agent_lock() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::{install_mock_agent, LuaHostError, SharedHardware};
    use std::sync::{Arc, Mutex};

    let agent = install_mock_agent("SET_COOLING").unwrap(); // SharedAgent
                                                            // Poison the mutex by panicking while it is held, caught on this thread
                                                            // (`MiniAgent` is not `Send`, so a spawned thread would not compile).
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = agent.lock().unwrap();
        panic!("intentional agent poison");
    }));
    assert!(agent.is_poisoned(), "the agent lock should be poisoned");

    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let mut vm = magent_lua::piccolo_vm::PiccoloVm::new(hw, agent);
    let err = vm.run_script("agent.reason('hi', 'advice')").unwrap_err();
    assert!(
        matches!(err, LuaHostError::Lua(_)),
        "poisoned agent must surface as a Lua error: {err:?}"
    );
    assert!(
        err.to_string().contains("poisoned"),
        "error should mention the poisoned agent lock: {err}"
    );
}

// ===========================================================================
// AppRuntime<PiccoloVm> end-to-end runtime behaviour (supervisor surface)
// ===========================================================================

/// `AppRuntime<PiccoloVm>::health()` reports tick_count, error_count,
/// uptime_ms — same supervisor contract as the mlua path.  This proves the
/// runtime trait is engine-agnostic, not just the dispatch surface.
#[test]
fn piccolo_runtime_health_snapshot() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::piccolo_vm::PiccoloVm;
    use magent_lua::runtime::AppRuntime;
    use magent_lua::{install_mock_agent, SharedHardware};
    use std::sync::{Arc, Mutex};

    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let agent = install_mock_agent("FAN_ON").unwrap();
    let mut app = AppRuntime::new(PiccoloVm::new(hw.clone(), agent), hw);
    app.boot("function on_tick() return 'FAN_ON' end").unwrap();

    let t = app.tick().unwrap();
    assert!(t.dispatched);

    let h = app.health(std::time::Duration::from_millis(100));
    assert_eq!(h.tick_count, 1);
    assert_eq!(h.error_count, 0);
    assert!(!h.stale);
    assert!(h.last_error.is_none());
}

/// `AppRuntime<PiccoloVm>::reload()` must reset the supervisor counters
/// (tick_count, error_count, last_error) — same contract as mlua.
#[test]
fn piccolo_runtime_reload_resets_counters() {
    use magent_lua::hardware::SimHardware;
    use magent_lua::piccolo_vm::PiccoloVm;
    use magent_lua::runtime::AppRuntime;
    use magent_lua::{install_mock_agent, SharedHardware};
    use std::sync::{Arc, Mutex};

    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let mut app = AppRuntime::new(
        PiccoloVm::new(hw.clone(), install_mock_agent("FAN_ON").unwrap()),
        hw,
    );
    app.boot("function on_tick() error('boom') end").unwrap();
    let _ = app.tick().unwrap_err();
    let _ = app.tick().unwrap_err();

    app.reload("function on_tick() return '' end").unwrap();
    let h = app.health(std::time::Duration::from_millis(100));
    assert_eq!(h.tick_count, 0, "reload resets tick_count");
    assert_eq!(h.error_count, 0, "reload resets error_count");
    assert!(h.last_error.is_none(), "reload clears last_error");
}

/// `run_until_stop` honours a pre-set stop flag on the pure-Rust engine
/// (mirrors `mlua` test `run_until_stop_stops_immediately_when_stop_flag_pre_set`).
#[test]
fn piccolo_runtime_run_until_stop_pre_set_exits_immediately() {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use magent_lua::hardware::SimHardware;
    use magent_lua::piccolo_vm::PiccoloVm;
    use magent_lua::runtime::AppRuntime;
    use magent_lua::{install_mock_agent, SharedHardware};
    use std::sync::{Arc, Mutex};

    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let mut app = AppRuntime::new(
        PiccoloVm::new(hw.clone(), install_mock_agent("FAN_ON").unwrap()),
        hw,
    );
    app.boot("function on_tick() return '' end").unwrap();
    app.stop_flag().store(true, Ordering::Release);
    let ticks = app.run_until_stop(Duration::ZERO, None);
    assert_eq!(ticks, 0, "pre-set stop must skip the loop");
}

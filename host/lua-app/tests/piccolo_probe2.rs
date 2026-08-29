//! Smoke tests for `string.match` shim in `PiccoloVm`.
//!
//! Built only when the `piccolo` feature is on:
//! `cargo test -p magent-lua --features piccolo --no-default-features --test piccolo_probe2`

#![cfg(feature = "piccolo")]

use magent_lua::hardware::SimHardware;
use magent_lua::piccolo_vm::PiccoloVm;
use magent_lua::{install_mock_agent, SharedHardware};
use std::sync::{Arc, Mutex};

fn run_and_call(vm: &mut PiccoloVm, script: &str) -> String {
    vm.run_script(script)
        .unwrap_or_else(|e| panic!("script:\n{script}\nerror: {e}"));
    vm.call("on_script", &[])
        .unwrap_or_else(|e| panic!("call on_script failed: {e}"))
}

fn fresh_vm() -> PiccoloVm {
    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let agent = install_mock_agent("OK").unwrap();
    PiccoloVm::new(hw, agent)
}

fn assert_script_returns(vm: &mut PiccoloVm, script: &str, expected: &str) {
    let actual = run_and_call(vm, script);
    assert_eq!(
        actual, expected,
        "\nscript:\n{script}\nexpected: {expected:?}\nactual:   {actual:?}"
    );
}

#[test]
fn match_simple_word_capture() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.match("hello", "(%w+)") end"#,
        "hello",
    );
}

#[test]
fn match_digit_capture_from_action() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script()
            return string.match("SET_COOLING_PULSE:60", "SET_COOLING_PULSE:(%d+)")
        end"#,
        "60",
    );
}

#[test]
fn match_then_tonumber() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script()
            local s = string.match("SET_COOLING_PULSE:60", "SET_COOLING_PULSE:(%d+)")
            local n = tonumber(s)
            return tostring(n)
        end"#,
        "60",
    );
}

#[test]
fn match_e2e_like_greenhouse() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script()
            local s = "SET_COOLING_PULSE:60"
            local m = string.match(s, "SET_COOLING_PULSE:(%d+)")
            local n = tonumber(m)
            return tostring(n)
        end"#,
        "60",
    );
}

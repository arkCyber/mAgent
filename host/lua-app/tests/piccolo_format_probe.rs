//! Smoke tests for `string.format` shim in `PiccoloVm`.
//!
//! Built only when the `piccolo` feature is on:
//! `cargo test -p magent-lua --features piccolo --no-default-features --test piccolo_format_probe`

#![cfg(feature = "piccolo")]

use magent_lua::hardware::SimHardware;
use magent_lua::piccolo_vm::PiccoloVm;
use magent_lua::{install_mock_agent, SharedHardware};
use std::sync::{Arc, Mutex};

/// Run `script` (which must define `function on_script() ... end` returning a
/// string), then call `on_script` and assert its return value equals
/// `expected`.  Panics with full context on mismatch or runtime error.
fn assert_script_returns(vm: &mut PiccoloVm, script: &str, expected: &str) {
    vm.run_script(script)
        .unwrap_or_else(|e| panic!("script:\n{script}\nerror: {e}"));
    let actual = vm
        .call("on_script", &[])
        .unwrap_or_else(|e| panic!("call on_script failed: {e}"));
    assert_eq!(
        actual, expected,
        "\nscript:\n{script}\nexpected: {expected:?}\nactual:   {actual:?}"
    );
}

fn fresh_vm() -> PiccoloVm {
    let hw: SharedHardware = Arc::new(Mutex::new(SimHardware::default()));
    let agent = install_mock_agent("OK").unwrap();
    PiccoloVm::new(hw, agent)
}

#[test]
fn format_no_args() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.format("hello") end"#,
        "hello",
    );
}

#[test]
fn format_s_conversion() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.format("val=%s", "test") end"#,
        "val=test",
    );
}

#[test]
fn format_d_conversion() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.format("%d", 42) end"#,
        "42",
    );
}

#[test]
fn format_percent_escape() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.format("%% is percent") end"#,
        "% is percent",
    );
}

#[test]
fn format_greenhouse_flow() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"
        function on_script()
            local m = string.match("SET_COOLING_PULSE:60", "SET_COOLING_PULSE:(%d+)")
            local d = tonumber(m)
            return string.format("SET_COOLING_PULSE:%d", d)
        end
        "#,
        "SET_COOLING_PULSE:60",
    );
}

#[test]
fn format_trailing_percent_literal() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.format("val=%." ) end"#,
        "val=%.",
    );
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.format("val=%.5.") end"#,
        "val=%.5.",
    );
}

#[test]
fn match_dangling_close_paren_is_literal() {
    let mut vm = fresh_vm();
    // A `)` outside any capture is a literal character; it should NOT be
    // silently swallowed. "hello)" matches %w+ with capture "hello".
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.match("hello)", "(%w+)") end"#,
        "hello",
    );
}

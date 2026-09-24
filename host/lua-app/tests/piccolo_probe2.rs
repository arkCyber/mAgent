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

// ---------------------------------------------------------------------------
// Additional cross-engine parity probes for `string.match` / `tonumber`.
// ---------------------------------------------------------------------------

/// `%w+` should match an alphanumeric word in `hello world`.
#[test]
fn match_word_class_capture() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.match("hello world", "(%w+)") end"#,
        "hello",
    );
}

/// `%s+` should match a run of whitespace (whole-match path).
#[test]
fn match_space_class_returns_whitespace() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.match("a   b", "(%s+)") end"#,
        "   ",
    );
}

/// `%a+` should match alphabetic-only text.
#[test]
fn match_alpha_class_capture() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.match("abc123", "(%a+)") end"#,
        "abc",
    );
}

/// `%D` (non-digit) class — negation form must also work.
#[test]
fn match_negated_digit_class() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.match("abc1", "(%D+)") end"#,
        "abc",
    );
}

/// `tonumber("-7")` must parse to an integer, matching standard Lua.
#[test]
fn tonumber_parses_negative_integer() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return tostring(tonumber("-7")) end"#,
        "-7",
    );
}

/// `tonumber("3.14")` must return a float (non-integral, not coerced to int).
#[test]
fn tonumber_parses_fractional() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return tostring(tonumber("3.14")) end"#,
        "3.14",
    );
}

/// `tonumber("ff", 16)` must parse a hex literal as an integer.
#[test]
fn tonumber_with_radix_parses_hex() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return tostring(tonumber("ff", 16)) end"#,
        "255",
    );
}

/// `tonumber("garbage")` must return nil, not error.  We compare with the
/// raw equality operator instead of `tostring()` because piccolo 0.3.x's
/// `tostring` chokes on nil.
#[test]
fn tonumber_returns_nil_on_garbage() {
    let mut vm = fresh_vm();
    vm.run_script(
        r#"function on_script() local n = tonumber("garbage") if n == nil then return "is_nil" else return "is_not_nil" end end"#,
    )
    .unwrap();
    let actual = vm.call("on_script", &[]).unwrap();
    assert_eq!(actual, "is_nil", "garbage must parse to nil");
}

/// An `init` index landing inside a multi-byte code point used to panic the
/// VM (`&s[start..]` with `start` off a char boundary). `温度` is 3 bytes per
/// character, so `init = 2` points at byte 1 — mid-character. The shim must
/// back up to the boundary rather than abort.
#[test]
fn match_init_inside_codepoint_does_not_panic() {
    let mut vm = fresh_vm();
    assert_script_returns(
        &mut vm,
        r#"function on_script() return string.match("温度", "(.+)", 2) end"#,
        "温度",
    );
}
